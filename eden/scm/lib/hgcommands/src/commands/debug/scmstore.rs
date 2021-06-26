/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::io::Write;

use async_runtime::{block_on, stream_to_iter as block_on_stream};
use clidispatch::errors;
use configparser::config::ConfigSet;
use revisionstore::scmstore::{
    file_to_async_key_stream, FileAttributes, FileStoreBuilder, TreeStoreBuilder,
};
use types::Key;

use super::define_flags;
use super::Repo;
use super::Result;
use super::IO;

define_flags! {
    pub struct DebugScmStoreOpts {
        /// Run the python version of the command instead (actually runs mostly in rust, but uses store constructed for python, with legacy fallback).
        python: bool,

        /// Fetch mode (file or tree)
        mode: String,

        /// Input file containing keys to fetch (hgid,path separated by newlines)
        path: String,
    }
}

enum FetchMode {
    File,
    Tree,
}

pub fn run(opts: DebugScmStoreOpts, io: &IO, repo: Repo) -> Result<u8> {
    if opts.python {
        return Err(errors::FallbackToPython.into());
    }

    let mode = match opts.mode.as_ref() {
        "file" => FetchMode::File,
        "tree" => FetchMode::Tree,
        _ => return Err(errors::Abort("'mode' must be one of 'file' or 'tree'".into()).into()),
    };

    let keys: Vec<_> =
        block_on_stream(block_on(file_to_async_key_stream(opts.path.into()))?).collect();

    let config = repo.config();

    match mode {
        FetchMode::File => fetch_files(io, &config, keys)?,
        FetchMode::Tree => fetch_trees(io, &config, keys)?,
    }

    Ok(0)
}

fn fetch_files(io: &IO, config: &ConfigSet, keys: Vec<Key>) -> Result<()> {
    let file_builder = FileStoreBuilder::new(&config);
    let store = file_builder.build()?;

    let mut stdout = io.output();

    let fetch_result = store.fetch(
        keys.into_iter(),
        FileAttributes {
            content: true,
            aux_data: true,
        },
    );

    for (_, file) in fetch_result.complete.into_iter() {
        write!(stdout, "Successfully fetched file: {:#?}\n", file)?;
    }
    for (key, _) in fetch_result.incomplete.into_iter() {
        write!(stdout, "Failed to fetch file: {:#?}\n", key)?;
    }

    Ok(())
}

fn fetch_trees(io: &IO, config: &ConfigSet, keys: Vec<Key>) -> Result<()> {
    let mut tree_builder = TreeStoreBuilder::new(config);
    tree_builder = tree_builder.suffix("manifests");
    let store = tree_builder.build()?;

    let mut stdout = io.output();

    let fetch_result = store.fetch_batch(keys.into_iter())?;

    for complete in fetch_result.complete.into_iter() {
        write!(stdout, "Successfully fetched tree: {:#?}\n", complete)?;
    }
    for incomplete in fetch_result.incomplete.into_iter() {
        write!(stdout, "Failed to fetch tree: {:#?}\n", incomplete)?;
    }

    Ok(())
}

pub fn name() -> &'static str {
    "debugscmstore"
}

pub fn doc() -> &'static str {
    "test file and tree fetching using scmstore"
}
