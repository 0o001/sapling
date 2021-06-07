/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{anyhow, Result};
use configparser::config::ConfigSet;
use hostname::get_hostname;
use serde::Serialize;

#[cfg(fbcode_build)]
mod facebook;

#[cfg(fbcode_build)]
use facebook::{get_fb_client_info, FbClientInfo};

#[cfg(not(fbcode_build))]
#[derive(Serialize, Debug)]
pub struct FbClientInfo {}

#[cfg(not(fbcode_build))]
fn get_fb_client_info() -> FbClientInfo {
    FbClientInfo {}
}

#[derive(Serialize, Debug)]
pub struct ClientInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub u64token: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(flatten)]
    pub fb: FbClientInfo,
}

impl ClientInfo {
    pub fn new(config: &ConfigSet) -> Result<Self> {
        let fb = get_fb_client_info();

        let u64token = config.get_opt::<u64>("ratelim", "u64token")?;
        let hostname = get_hostname().ok();

        Ok(ClientInfo {
            u64token,
            hostname,
            fb,
        })
    }

    pub fn into_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| anyhow!(e))
    }
}
