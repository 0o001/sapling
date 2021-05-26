/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::anyhow;
use async_trait::async_trait;
use context::CoreContext;
use megarepo_configs::types::{SyncConfigVersion, SyncTargetConfig, Target};
use megarepo_error::MegarepoError;
use slog::{info, Logger};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::MononokeMegarepoConfigs;

#[derive(Clone)]
pub struct TestMononokeMegarepoConfigs {
    config_versions: Arc<Mutex<HashMap<(Target, SyncConfigVersion), SyncTargetConfig>>>,
}

impl TestMononokeMegarepoConfigs {
    pub fn new(logger: &Logger) -> Self {
        info!(logger, "Creating a new TestMononokeMegarepoConfigs");
        Self {
            config_versions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn add(&mut self, key: (Target, SyncConfigVersion), target: SyncTargetConfig) {
        let mut config_versions = self.config_versions.lock().unwrap();
        config_versions.insert(key, target);
    }
}

#[async_trait]
impl MononokeMegarepoConfigs for TestMononokeMegarepoConfigs {
    fn get_target_config_versions(
        &self,
        _ctx: CoreContext,
        _target: Target,
    ) -> Result<Vec<SyncConfigVersion>, MegarepoError> {
        unimplemented!("TestMononokeMegarepoConfigs::get_target_config_versions")
    }

    fn get_config_by_version(
        &self,
        _ctx: CoreContext,
        target: Target,
        version: SyncConfigVersion,
    ) -> Result<SyncTargetConfig, MegarepoError> {
        let config_versions = self.config_versions.lock().unwrap();
        config_versions
            .get(&(target.clone(), version.clone()))
            .cloned()
            .ok_or_else(|| anyhow!("{:?} not found", (target, version)))
            .map_err(MegarepoError::internal)
    }

    async fn add_target_with_config_version(
        &self,
        _ctx: CoreContext,
        config: SyncTargetConfig,
    ) -> Result<(), MegarepoError> {
        let mut config_versions = self.config_versions.lock().unwrap();
        let key = (config.target.clone(), config.version.clone());
        config_versions.insert(key, config);
        Ok(())
    }

    async fn add_config_version(
        &self,
        _ctx: CoreContext,
        _config: SyncTargetConfig,
    ) -> Result<(), MegarepoError> {
        unimplemented!("TestMononokeMegarepoConfigs::add_config_version")
    }
}
