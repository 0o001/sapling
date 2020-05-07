/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#[cfg(fbcode_build)]
mod facebook;
#[cfg(not(fbcode_build))]
mod oss;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

pub type ArcGlobalTimeWindowCounter = Arc<dyn GlobalTimeWindowCounter + Send + Sync + 'static>;
pub type BoxGlobalTimeWindowCounter = Box<dyn GlobalTimeWindowCounter + Send + Sync + 'static>;

#[async_trait]
pub trait GlobalTimeWindowCounter {
    async fn get(&self, time_window: u32) -> Result<f64>;

    fn bump(&self, value: f64);
}

pub struct GlobalTimeWindowCounterBuilder {}
