/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![allow(non_camel_case_types)]

//! Utilities to make async <-> Python integration easier.
//!
//! The `TStream` type provides easy conversion between Rust `Stream` and Python
//! objects.
//!
//! The `PyFuture` type provides a way to export Rust `Future` to
//! Python.

mod future;
mod stream;

pub use future::future as PyFuture;
pub use stream::TStream;

// Re-export.
pub use anyhow;
pub use async_runtime;
pub use cpython_ext;
pub use cpython_ext::cpython;
pub use futures;
