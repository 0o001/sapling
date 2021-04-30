/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![allow(non_camel_case_types)]

use anyhow::Result;
use async_runtime::try_block_unless_interrupted;
use checkout::{Action, ActionMap, Checkout, CheckoutPlan, Conflict, Merge, MergeResult};
use cpython::*;
use cpython_ext::{ExtractInnerRef, PyNone, PyPathBuf, ResultPyErrExt};
use manifest_tree::Diff;
use manifest_tree::TreeManifest;
use pathmatcher::{AlwaysMatcher, Matcher};
use pyconfigparser::config;
use pymanifest::treemanifest;
use pypathmatcher::PythonMatcher;
use pyrevisionstore::{contentstore, filescmstore};
use pystatus::status as PyStatus;
use pytreestate::treestate as PyTreeState;
use std::collections::HashMap;
use std::time::SystemTime;
use tracing::warn;
use treestate::filestate::{FileStateV2, StateFlags};
use types::RepoPath;
use vfs::VFS;

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "checkout"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add_class::<checkoutplan>(py)?;
    m.add_class::<mergeresult>(py)?;
    m.add_class::<manifestbuilder>(py)?;
    Ok(m)
}

py_class!(class checkoutplan |py| {
    data plan: CheckoutPlan;

    def __new__(
        _cls,
        config: &config,
        root: PyPathBuf,
        current_manifest: &treemanifest,
        target_manifest: &treemanifest,
        matcher: Option<PyObject> = None,
        // If sparse profile changes, contains Some((old_sparse_matcher, new_sparse_matcher))
        sparse_change: Option<(PyObject, PyObject)> = None,
        progress_path: Option<PyPathBuf> = None,
    ) -> PyResult<checkoutplan> {
        let config = config.get_cfg(py);
        let matcher: Box<dyn Matcher> = match matcher {
            None => Box::new(AlwaysMatcher::new()),
            Some(pyobj) => Box::new(PythonMatcher::new(py, pyobj)),
        };

        let current = current_manifest.borrow_underlying(py);
        let target = target_manifest.borrow_underlying(py);
        let diff = Diff::new(&current, &target, &matcher);
        let mut actions = ActionMap::from_diff(diff).map_pyerr(py)?;
        if let Some((old_sparse_matcher, new_sparse_matcher)) = sparse_change {
            let old_matcher = Box::new(PythonMatcher::new(py, old_sparse_matcher));
            let new_matcher = Box::new(PythonMatcher::new(py, new_sparse_matcher));
            actions = actions.with_sparse_profile_change(&old_matcher, &new_matcher, &*target).map_pyerr(py)?;
        }
        let vfs = VFS::new(root.to_path_buf()).map_pyerr(py)?;
        let checkout = Checkout::from_config(vfs, &config).map_pyerr(py)?;
        let mut plan = checkout.plan_action_map(actions);
        if let Some(progress_path) = progress_path {
            plan.add_progress(progress_path.to_path_buf()).map_pyerr(py)?;
        }
        checkoutplan::create_instance(py, plan)
    }

    def check_unknown_files(&self, manifest: &treemanifest, scmstore: &filescmstore, state: &PyTreeState) -> PyResult<Vec<String>> {
        let plan = self.plan(py);
        let state = state.get_state(py);
        let manifest = manifest.borrow_underlying(py).clone();
        let store = scmstore.extract_inner_ref(py).clone();
        let unknown = py.allow_threads(move || -> Result<_> {
            let mut state = state.lock();
            try_block_unless_interrupted(
            plan.check_unknown_files(&manifest, store, &mut state))
        }).map_pyerr(py)?;
        Ok(unknown.into_iter().map(|p|p.to_string()).collect())
    }

    def check_conflicts(&self, status: &PyStatus) -> PyResult<Vec<String>> {
        let status = status.extract_inner_ref(py);
        let plan = self.plan(py);
        let conflicts = plan.check_conflicts(status);
        let conflicts = conflicts.into_iter().map(ToString::to_string).collect();
        Ok(conflicts)
    }

    def apply(&self, content_store: &contentstore) -> PyResult<PyNone> {
        let store = content_store.extract_inner_ref(py);
        let plan = self.plan(py);
        py.allow_threads(|| try_block_unless_interrupted(
            plan.apply_remote_data_store(store)
        )).map_pyerr(py)?;
        Ok(PyNone)
    }

    def apply_dry_run(&self, content_store: &contentstore) -> PyResult<(usize, u64)> {
        let store = content_store.extract_inner_ref(py);
        let plan = self.plan(py);
        py.allow_threads(|| try_block_unless_interrupted(
            plan.apply_remote_data_store_dry_run(store)
        )).map_pyerr(py)
    }

    def apply_scmstore_dry_run(&self, scmstore: &filescmstore) -> PyResult<(usize, u64)> {
        let store = scmstore.extract_inner_ref(py).clone();
        let plan = self.plan(py);
        py.allow_threads(|| try_block_unless_interrupted(
            plan.apply_read_store_dry_run(store)
        )).map_pyerr(py)
    }

    def apply_scmstore(&self, scmstore: &filescmstore) -> PyResult<PyNone> {
        let store = scmstore.extract_inner_ref(py).clone();
        let plan = self.plan(py);
        py.allow_threads(|| try_block_unless_interrupted(
            plan.apply_read_store(store)
        )).map_pyerr(py)?;
        Ok(PyNone)
    }

    def stats(&self) -> PyResult<(usize, usize, usize, usize)> {
        let plan = self.plan(py);
        let (updated, removed) = plan.stats();
        let (merged, unresolved) = (0, 0);

        Ok((updated, merged, removed, unresolved))
    }

    def record_updates(&self, state: &PyTreeState) -> PyResult<PyNone> {
        let plan = self.plan(py);
        let vfs = plan.vfs();
        let state = state.get_state(py);
        py.allow_threads(move || -> Result<()> {
            let mut state = state.lock();

            for removed in plan.removed_files() {
                state.remove(removed)?;
            }

            for updated in plan.updated_content_files().chain(plan.updated_meta_files()) {
                let fstate = file_state(vfs, updated)?;
                state.insert(updated, &fstate)?;
            }

            Ok(())
        }).map_pyerr(py)?;

        Ok(PyNone)
    }

    def __str__(&self) -> PyResult<PyString> {
        Ok(PyString::new(py, &self.plan(py).to_string()))
    }

    // This function is not efficient, only good for debug commands
    def __len__(&self) -> PyResult<usize> {
        Ok(self.plan(py).all_files().count())
    }

});

py_class!(class mergeresult |py| {
    data merge_result: MergeResult<TreeManifest>;

    def __new__(
        _cls,
        src_manifest: &treemanifest,
        dst_manifest: &treemanifest,
        ancestor_manifest: &treemanifest,
        // matcher: Option<PyObject> = None,
        // If sparse profile changes, contains Some((old_sparse_matcher, new_sparse_matcher))
        // sparse_change: Option<(PyObject, PyObject)> = None,
    ) -> PyResult<mergeresult> {
        let src = src_manifest.borrow_underlying(py);
        let dst = dst_manifest.borrow_underlying(py);
        let ancestor = ancestor_manifest.borrow_underlying(py);
        let merge_result = Merge{}.merge(&*src, &*dst, &*ancestor).map_pyerr(py)?;
        mergeresult::create_instance(py, merge_result)
    }

    def __str__(&self) -> PyResult<PyString> {
        Ok(PyString::new(py, &self.merge_result(py).to_string()))
    }

    def pymerge_actions(&self) -> PyResult<Option<HashMap<String, (&'static str, (&'static str, bool), &'static str)>>> {
        let actions = self.merge_result(py).try_actions();
        if let Some(actions) = actions {
            Ok(Some(actions.iter().map(|(k,v)|(k.to_string(), v.pymerge_action())).collect()))
        } else {
            Ok(None)
        }
    }

    def manifestbuilder(&self) -> PyResult<Option<manifestbuilder>> {
        let mut modifiedconflicts = vec![];
        for (path, conflict) in self.merge_result(py).conflicts().iter() {
            match conflict {
                Conflict::BothChanged{ancestor, dest, src} => {
                    if ancestor.is_some() && src.file_type == dest.file_type {
                        modifiedconflicts.push(path.to_string()); // both modified
                    } else {
                        // This is either both created(ancestor.is_none), no way to do 3-way merge
                        // Or, file type differs between src and dst - needs special handling
                        return Ok(None);
                    }
                },
                _ => return Ok(None)
            }
        }
        let actions = self.merge_result(py).actions();
        let actions = actions.clone();
        Ok(Some(manifestbuilder::create_instance(py, actions, modifiedconflicts)?))
    }

    def conflict_paths(&self) -> PyResult<Vec<String>> {
        Ok(self.merge_result(py).conflicts().keys().map(|k|k.to_string()).collect())
    }
});

py_class!(class manifestbuilder |py| {
    data actions: ActionMap;
    data _modifiedconflicts: Vec<String>;

    def removed(&self) -> PyResult<Vec<String>> {
        let actions = self.actions(py);
        Ok(actions.iter().filter_map(|(f, a)|
            if matches!(a, Action::Remove) {
                Some(f.to_string())
            } else {
                None
            })
        .collect())
    }

    def modified(&self) -> PyResult<Vec<String>> {
        let actions = self.actions(py);
        Ok(actions.iter().filter_map(|(f, a)|
            if !matches!(a, Action::Remove) {
                Some(f.to_string())
            } else {
                None
            })
        .collect())
    }

    def modifiedconflicts(&self) -> PyResult<Vec<String>> {
        Ok(self._modifiedconflicts(py).clone())
    }
});

fn file_state(vfs: &VFS, path: &RepoPath) -> Result<FileStateV2> {
    let meta = vfs.metadata(path)?;
    #[cfg(unix)]
    let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
    #[cfg(windows)]
    let mode = 0o644; // todo figure this out
    let mtime = meta
        .modified()?
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();
    let mtime = truncate_u64("mtime", path, mtime);
    let size = meta.len();
    let size = truncate_u64("size", path, size);
    let state = StateFlags::EXIST_P1 | StateFlags::EXIST_NEXT;
    Ok(FileStateV2 {
        mode,
        size,
        mtime,
        state,
        copied: None,
    })
}

fn truncate_u64(f: &str, path: &RepoPath, v: u64) -> i32 {
    const RANGE_MASK: u64 = 0x7FFFFFFF;
    let truncated = v & RANGE_MASK;
    if truncated != v {
        warn!("{} for {} is truncated {}=>{}", f, path, v, truncated);
    }
    truncated as i32
}
