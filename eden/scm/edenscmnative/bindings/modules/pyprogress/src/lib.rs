/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Rust bindings for Mercurial's Python `progress` module.
//!
//! This crate provides wrappers around Mercurial's Python progress bar objects
//! so that they may be used by pure Rust code, as well as compatibility shims
//! so that Python code can also use the Rust progress API. This will enable
//! the eventual transition to a pure Rust progress bar implementation.

#![allow(non_camel_case_types)]

use cpython::*;

pub use rust::PyProgressFactory;

mod model;
mod python;
mod render;
mod rust;

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "progress"].join(".");
    let m = PyModule::new(py, &name)?;

    m.add_class::<python::bar>(py)?;
    m.add_class::<python::spinner>(py)?;

    let model_mod = PyModule::new(py, &format!("{}.model", name))?;
    model_mod.add_class::<model::ProgressBar>(py)?;
    model_mod.add_class::<model::CacheStats>(py)?;
    m.add(py, "model", model_mod)?;

    let render_mod = PyModule::new(py, &format!("{}.model", name))?;
    use render::debug;
    use render::simple;
    render_mod.add(py, "simple", py_fn!(py, simple()))?;
    render_mod.add(py, "debug", py_fn!(py, debug()))?;
    m.add(py, "render", render_mod)?;

    Ok(m)
}
