/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

mod builder;
mod connections;
mod local_cache;
mod reader;
mod remote_cache;
mod shards;
mod sql_timeout_knobs;
mod structs;
mod writer;

#[cfg(test)]
mod test;

use anyhow::{Context, Error};
use cloned::cloned;
use context::CoreContext;
use filenodes::{FilenodeInfo, FilenodeRangeResult, FilenodeResult, Filenodes, PreparedFilenode};
use futures::future::{FutureExt as _, TryFutureExt};
use futures_ext::{BoxFuture, FutureExt as _};
use mercurial_types::HgFileNodeId;
use mononoke_types::{RepoPath, RepositoryId};
use std::sync::Arc;
use thiserror::Error as DeriveError;

pub use builder::NewFilenodesBuilder;
pub use path_hash::PathHash;
use reader::FilenodesReader;
pub use sql_timeout_knobs::disable_sql_timeouts;
use writer::FilenodesWriter;

#[derive(Debug, DeriveError)]
pub enum ErrorKind {
    #[error("Internal error: failure while fetching file node {0} {1}")]
    FailFetchFilenode(HgFileNodeId, RepoPath),

    #[error("Internal error: failure while fetching file nodes for {0}")]
    FailFetchFilenodeRange(RepoPath),

    #[error("Internal error: failure while inserting filenodes")]
    FailAddFilenodes,
}

#[derive(Clone)]
pub struct NewFilenodes {
    reader: Arc<FilenodesReader>,
    writer: Arc<FilenodesWriter>,
}

impl Filenodes for NewFilenodes {
    fn add_filenodes(
        &self,
        ctx: CoreContext,
        info: Vec<PreparedFilenode>,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<()>, Error> {
        cloned!(self.writer);

        async move {
            let ret = writer
                .insert_filenodes(&ctx, repo_id, info, false /* replace */)
                .await
                .with_context(|| ErrorKind::FailAddFilenodes)?;
            Ok(ret)
        }
        .boxed()
        .compat()
        .boxify()
    }

    fn add_or_replace_filenodes(
        &self,
        ctx: CoreContext,
        info: Vec<PreparedFilenode>,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<()>, Error> {
        cloned!(self.writer);

        async move {
            let ret = writer
                .insert_filenodes(&ctx, repo_id, info, true /* replace */)
                .await
                .with_context(|| ErrorKind::FailAddFilenodes)?;
            Ok(ret)
        }
        .boxed()
        .compat()
        .boxify()
    }

    fn get_filenode(
        &self,
        ctx: CoreContext,
        path: &RepoPath,
        filenode_id: HgFileNodeId,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<Option<FilenodeInfo>>, Error> {
        cloned!(self.reader, path);

        async move {
            let ret = reader
                .get_filenode(&ctx, repo_id, &path, filenode_id)
                .await
                .with_context(|| ErrorKind::FailFetchFilenode(filenode_id, path))?;
            Ok(ret)
        }
        .boxed()
        .compat()
        .boxify()
    }

    fn get_all_filenodes_maybe_stale(
        &self,
        ctx: CoreContext,
        path: &RepoPath,
        repo_id: RepositoryId,
        limit: Option<u64>,
    ) -> BoxFuture<FilenodeRangeResult<Vec<FilenodeInfo>>, Error> {
        cloned!(self.reader, path);

        async move {
            let ret = reader
                .get_all_filenodes_for_path(&ctx, repo_id, &path, limit)
                .await
                .with_context(|| ErrorKind::FailFetchFilenodeRange(path))?;
            Ok(ret)
        }
        .boxed()
        .compat()
        .boxify()
    }

    fn prime_cache(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        filenodes: &[PreparedFilenode],
    ) {
        self.reader.prime_cache(ctx, repo_id, filenodes);
    }
}
