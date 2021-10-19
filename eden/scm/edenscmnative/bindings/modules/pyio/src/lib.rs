/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![allow(non_camel_case_types)]

use clidispatch::io::IO as RustIO;
use cpython::*;
use cpython_ext::wrap_rust_write;
use cpython_ext::PyNone;
use cpython_ext::ResultPyErrExt;
use pyconfigparser::config as PyConfig;
use std::cell::Cell;

pub fn init_module(py: Python, package: &str) -> PyResult<PyModule> {
    let name = [package, "io"].join(".");
    let m = PyModule::new(py, &name)?;
    m.add_class::<IO>(py)?;
    Ok(m)
}

py_class!(class IO |py| {
    data closed: Cell<bool>;

    @staticmethod
    def main() -> PyResult<IO> {
        Self::create_instance(py, Cell::new(false))
    }

    /// Start the stream pager.
    def start_pager(&self, config: PyConfig) -> PyResult<PyNone> {
        self.check_closed(py)?;
        let io = RustIO::main().map_pyerr(py)?;
        let config = &config.get_cfg(py);
        io.start_pager(config).map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Test if the pager is active.
    def is_pager_active(&self) -> PyResult<bool> {
        let io = RustIO::main().map_pyerr(py)?;
        Ok(io.is_pager_active())
    }

    /// Write to pager's main buffer. Text should be in utf-8.
    def write(&self, bytes: PyBytes) -> PyResult<PyNone> {
        self.check_closed(py)?;
        let io = RustIO::main().map_pyerr(py)?;
        io.write(bytes.data(py)).map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Write to pager's stderr buffer. Text should be in utf-8.
    def write_err(&self, bytes: PyBytes) -> PyResult<PyNone> {
        self.check_closed(py)?;
        let io = RustIO::main().map_pyerr(py)?;
        io.write_err(bytes.data(py)).map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Set the progress content.
    def set_progress(&self, message: &str) -> PyResult<PyNone> {
        self.check_closed(py)?;
        let io = RustIO::main().map_pyerr(py)?;
        io.set_progress(message).map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Wait for the pager to end.
    def wait_pager(&self) -> PyResult<PyNone> {
        self.closed(py).set(true);
        let io = RustIO::main().map_pyerr(py)?;
        io.wait_pager().map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Return the output stream as a Python object with "write" method.
    def output(&self) -> PyResult<PyObject> {
        let io = RustIO::main().map_pyerr(py)?;
        Ok(wrap_rust_write(py, io.output())?.into_object())
    }

    /// Return the error stream as a Python object with "write" method.
    def error(&self) -> PyResult<PyObject> {
        let io = RustIO::main().map_pyerr(py)?;
        Ok(wrap_rust_write(py, io.error())?.into_object())
    }

    /// Flush pending changes.
    def flush(&self) -> PyResult<PyNone> {
        let io = RustIO::main().map_pyerr(py)?;
        io.flush().map_pyerr(py)?;
        Ok(PyNone)
    }

    /// Disable progress output.
    def disable_progress(&self, disabled: bool = true) -> PyResult<PyNone> {
        let io = RustIO::main().map_pyerr(py)?;
        io.disable_progress(disabled).map_pyerr(py)?;
        Ok(PyNone)
    }
});

impl IO {
    fn check_closed(&self, py: Python) -> PyResult<PyNone> {
        if self.closed(py).get() {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "pager was closed",
            ))
            .map_pyerr(py)
        } else {
            Ok(PyNone)
        }
    }
}
