/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::path::{Path, PathBuf};

use anyhow::Result;
use thiserror::Error;

use configparser::{config::ConfigSet, hg::ConfigSetHgExt};
use util::path::create_dir;

#[derive(Error, Debug)]
pub enum Error {
    #[error("could not find config option {0}")]
    ConfigNotSet(String),
}

pub fn get_str_config(config: &ConfigSet, section: &str, name: &str) -> Result<String> {
    let name = config
        .get(section, name)
        .ok_or_else(|| Error::ConfigNotSet(format!("{}.{}", section, name).into()))?;
    Ok(String::from_utf8(name.to_vec())?)
}

pub fn get_repo_name(config: &ConfigSet) -> Result<String> {
    get_str_config(config, "remotefilelog", "reponame")
}

fn get_cache_path(config: &ConfigSet) -> Result<PathBuf> {
    let reponame = get_repo_name(config)?;
    let config_path: PathBuf = config
        .get_or_default::<Option<_>>("remotefilelog", "cachepath")?
        .ok_or_else(|| Error::ConfigNotSet("remotefilelog.cachepath".into()))?;
    let mut path = PathBuf::new();
    path.push(config_path);
    create_dir(&path)?;
    path.push(reponame);
    create_dir(&path)?;
    Ok(path)
}

pub fn get_cache_packs_path(config: &ConfigSet, suffix: Option<&Path>) -> Result<PathBuf> {
    let mut path = get_cache_path(config)?;
    path.push("packs");
    create_dir(&path)?;
    if let Some(suffix) = suffix {
        path.push(suffix);
    }
    create_dir(&path)?;
    Ok(path)
}

pub fn get_cache_indexedlogdatastore_path(config: &ConfigSet) -> Result<PathBuf> {
    let mut path = get_cache_path(config)?;
    path.push("indexedlogdatastore");
    create_dir(&path)?;
    Ok(path)
}

pub fn get_cache_indexedloghistorystore_path(config: &ConfigSet) -> Result<PathBuf> {
    let mut path = get_cache_path(config)?;
    path.push("indexedloghistorystore");
    create_dir(&path)?;
    Ok(path)
}

pub fn get_local_packs_path(path: impl AsRef<Path>, suffix: Option<&Path>) -> Result<PathBuf> {
    let mut path = path.as_ref().to_owned();
    path.push("packs");
    create_dir(&path)?;

    if let Some(suffix) = suffix {
        path.push(suffix);
    }

    create_dir(&path)?;
    Ok(path)
}

fn get_lfs_path(store_path: impl AsRef<Path>) -> Result<PathBuf> {
    let mut path = store_path.as_ref().to_owned();
    path.push("lfs");
    create_dir(&path)?;

    Ok(path)
}

pub fn get_lfs_pointers_path(store_path: impl AsRef<Path>) -> Result<PathBuf> {
    let mut path = get_lfs_path(store_path)?;
    path.push("pointers");
    create_dir(&path)?;

    Ok(path)
}

pub fn get_lfs_blobs_path(store_path: impl AsRef<Path>) -> Result<PathBuf> {
    let mut path = get_lfs_path(store_path)?;
    path.push("objects");
    create_dir(&path)?;

    Ok(path)
}
