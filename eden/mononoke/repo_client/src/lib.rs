/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]
#![feature(async_closure)]

//! State for a single source control Repo

mod client;
mod errors;

pub use client::{fetch_treepack_part_input, gettreepack_entries, RepoClient, WireprotoLogging};
pub use mononoke_repo::{streaming_clone, MononokeRepo, MononokeRepoBuilder};
pub use repo_read_write_status::RepoReadWriteFetcher;
pub use unbundle::{PushRedirector, PushRedirectorArgs, CONFIGERATOR_PUSHREDIRECT_ENABLE};
