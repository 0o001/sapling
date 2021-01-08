/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![allow(non_camel_case_types)]

use std::cell::RefCell;

use anyhow::Error;
use cpython::*;

use cpython_ext::error::ResultPyErrExt;
use cpython_ext::PyPathBuf;
use pypathmatcher::UnsafePythonMatcher;
use pytreestate::treestate;
use workingcopy::filesystem::{
    ChangeType, PendingChangeResult, PendingChanges, PhysicalFileSystem,
};
use workingcopy::walker::{WalkError, Walker};

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "workingcopy"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add_class::<walker>(py)?;
    m.add_class::<pendingchanges>(py)?;
    m.add_class::<physicalfilesystem>(py)?;
    Ok(m)
}

py_class!(class physicalfilesystem |py| {
    data filesystem: RefCell<PhysicalFileSystem>;

    def __new__(_cls, root: PyPathBuf) -> PyResult<physicalfilesystem> {
        physicalfilesystem::create_instance(py, RefCell::new(PhysicalFileSystem::new(root.to_path_buf()).map_pyerr(py)?))
    }

    def pendingchanges(&self, pytreestate: treestate, pymatcher: PyObject, include_directories: bool, last_write: u32) -> PyResult<pendingchanges> {
        let matcher = UnsafePythonMatcher::new(pymatcher);
        let fs = self.filesystem(py);
        let treestate = pytreestate.get_state(py);
        let last_write = last_write.into();
        let pending = fs.borrow()
            .pending_changes(treestate, matcher, include_directories, last_write)
            .map_pyerr(py)?;
        pendingchanges::create_instance(py, RefCell::new(pending))
    }
});

py_class!(class pendingchanges |py| {
    data inner: RefCell<PendingChanges<UnsafePythonMatcher>>;

    def __iter__(&self) -> PyResult<Self> {
        Ok(self.clone_ref(py))
    }

    def __next__(&self) -> PyResult<Option<(PyPathBuf, bool)>> {
        loop {
            match self.inner(py).borrow_mut().next() {
                Some(Ok(change)) => {
                    if let PendingChangeResult::File(change_type) = change {
                        return Ok(Some(match change_type {
                            ChangeType::Changed(path) => (path.into(), true),
                            ChangeType::Deleted(path) => (path.into(), false),
                        }));
                    }
                },
                Some(Err(_)) => {
                    // TODO: Add error handling
                    continue
                },
                None => return Ok(None),
            };
        }
    }
});

py_class!(class walker |py| {
    data walker: RefCell<Walker<UnsafePythonMatcher>>;
    data _errors: RefCell<Vec<Error>>;
    def __new__(_cls, root: PyPathBuf, pymatcher: PyObject, include_directories: bool) -> PyResult<walker> {
        let matcher = UnsafePythonMatcher::new(pymatcher);
        let walker = Walker::new(root.to_path_buf(), matcher, include_directories).map_pyerr(py)?;
        walker::create_instance(py, RefCell::new(walker), RefCell::new(Vec::new()))
    }

    def __iter__(&self) -> PyResult<Self> {
        Ok(self.clone_ref(py))
    }

    def __next__(&self) -> PyResult<Option<PyPathBuf>> {
        loop {
            match self.walker(py).borrow_mut().next() {
                Some(Ok(path)) => {
                    return Ok(Some(PyPathBuf::from(path.as_ref())))
                },
                Some(Err(e)) => {
                    self._errors(py).borrow_mut().push(e)
                },
                None => return Ok(None),
            };
        }
    }

    def errors(&self) -> PyResult<Vec<(cpython_ext::Str, cpython_ext::Str)>> {
        Ok(self._errors(py).borrow().iter().map(|e| match e.downcast_ref::<WalkError>() {
            Some(e) => (e.filename().into(), e.message().into()),
            None => ("unknown".to_string().into(), e.to_string().into()),
        }).collect::<Vec<(cpython_ext::Str, cpython_ext::Str)>>())
    }

});
