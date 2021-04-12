/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! # configmodel
//!
//! Provides a trait definition for config reading.

pub mod config;
pub mod convert;
pub mod error;

pub use config::Config;
pub use config::ConfigExt;
pub use error::Error;
pub type Result<T> = std::result::Result<T, Error>;

// Re-export
pub use minibytes::Text;
