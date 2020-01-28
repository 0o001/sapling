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

use cpython_ext::PyPath;
use pypathmatcher::UnsafePythonMatcher;
use workingcopy::{WalkError, Walker};

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "workingcopy"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add_class::<walker>(py)?;
    Ok(m)
}

py_class!(class walker |py| {
    data walker: RefCell<Walker<UnsafePythonMatcher>>;
    data _errors: RefCell<Vec<Error>>;
    def __new__(_cls, root: PyPath, pymatcher: PyObject) -> PyResult<walker> {
        let matcher = UnsafePythonMatcher::new(pymatcher);
        walker::create_instance(py, RefCell::new(Walker::new(root.to_path_buf(), matcher)), RefCell::new(Vec::new()))
    }

    def __iter__(&self) -> PyResult<Self> {
        Ok(self.clone_ref(py))
    }

    def __next__(&self) -> PyResult<Option<PyPath>> {
        loop {
            match self.walker(py).borrow_mut().next() {
                Some(Ok(path)) => {
                    return Ok(Some(path.into()))
                },
                Some(Err(e)) => {
                    self._errors(py).borrow_mut().push(e)
                },
                None => return Ok(None),
            };
        }
    }

    def errors(&self) -> PyResult<Vec<(String, String)>> {
        Ok(self._errors(py).borrow().iter().map(|e| match e.downcast_ref::<WalkError>() {
            Some(e) => (e.filename(), e.message()),
            None => ("unknown".to_string(), e.to_string()),
        }).collect::<Vec<(String, String)>>())
    }

});
