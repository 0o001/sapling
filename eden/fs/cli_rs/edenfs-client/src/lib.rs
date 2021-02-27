/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use thrift_types::edenfs::client::EdenService;

pub mod instance;
mod utils;

pub use instance::{DaemonHealthy, EdenFsInstance};

pub type EdenFsClient = Arc<dyn EdenService + Sync>;
