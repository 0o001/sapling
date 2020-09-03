/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use async_runtime::block_on_future;
use edenapi_types::{CommitRevlogData, FileEntry, HistoryEntry, TreeEntry};
use types::{HgId, Key, RepoPathBuf};

use crate::api::{EdenApi, ProgressCallback};
use crate::errors::EdenApiError;
use crate::response::{BlockingFetch, ResponseMeta};

pub trait EdenApiBlocking: EdenApi {
    fn health_blocking(&self) -> Result<ResponseMeta, EdenApiError> {
        block_on_future(self.health())
    }

    fn files_blocking(
        &self,
        repo: String,
        keys: Vec<Key>,
        progress: Option<ProgressCallback>,
    ) -> Result<BlockingFetch<FileEntry>, EdenApiError> {
        BlockingFetch::from_async(self.files(repo, keys, progress))
    }

    fn history_blocking(
        &self,
        repo: String,
        keys: Vec<Key>,
        length: Option<u32>,
        progress: Option<ProgressCallback>,
    ) -> Result<BlockingFetch<HistoryEntry>, EdenApiError> {
        BlockingFetch::from_async(self.history(repo, keys, length, progress))
    }

    fn trees_blocking(
        &self,
        repo: String,
        keys: Vec<Key>,
        progress: Option<ProgressCallback>,
    ) -> Result<BlockingFetch<TreeEntry>, EdenApiError> {
        BlockingFetch::from_async(self.trees(repo, keys, progress))
    }

    fn complete_trees_blocking(
        &self,
        repo: String,
        rootdir: RepoPathBuf,
        mfnodes: Vec<HgId>,
        basemfnodes: Vec<HgId>,
        depth: Option<usize>,
        progress: Option<ProgressCallback>,
    ) -> Result<BlockingFetch<TreeEntry>, EdenApiError> {
        BlockingFetch::from_async(self.complete_trees(
            repo,
            rootdir,
            mfnodes,
            basemfnodes,
            depth,
            progress,
        ))
    }

    fn commit_revlog_data_blocking(
        &self,
        repo: String,
        hgids: Vec<HgId>,
        progress: Option<ProgressCallback>,
    ) -> Result<BlockingFetch<CommitRevlogData>, EdenApiError> {
        BlockingFetch::from_async(self.commit_revlog_data(repo, hgids, progress))
    }
}

impl<T: EdenApi + ?Sized> EdenApiBlocking for T {}
