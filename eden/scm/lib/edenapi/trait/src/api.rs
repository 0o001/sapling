/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::borrow::Borrow;
use std::sync::Arc;

use async_trait::async_trait;

use edenapi_types::CommitGraphEntry;
use edenapi_types::CommitKnownResponse;
use edenapi_types::{
    AnyId, BookmarkEntry, CloneData, CommitHashToLocationResponse, CommitLocationToHashRequest,
    CommitLocationToHashResponse, CommitRevlogData, EdenApiServerError, FileEntry, HistoryEntry,
    LookupResponse, TreeAttributes, TreeEntry,
};
use http_client::Progress;
use types::{HgId, Key, RepoPathBuf};

use crate::errors::EdenApiError;
use crate::response::{Fetch, ResponseMeta};

pub type ProgressCallback = Box<dyn FnMut(Progress) + Send + 'static>;

#[async_trait]
pub trait EdenApi: Send + Sync + 'static {
    async fn health(&self) -> Result<ResponseMeta, EdenApiError>;

    async fn files(
        &self,
        repo: String,
        keys: Vec<Key>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<FileEntry>, EdenApiError>;

    async fn history(
        &self,
        repo: String,
        keys: Vec<Key>,
        length: Option<u32>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<HistoryEntry>, EdenApiError>;

    async fn trees(
        &self,
        repo: String,
        keys: Vec<Key>,
        attributes: Option<TreeAttributes>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<Result<TreeEntry, EdenApiServerError>>, EdenApiError>;

    async fn complete_trees(
        &self,
        repo: String,
        rootdir: RepoPathBuf,
        mfnodes: Vec<HgId>,
        basemfnodes: Vec<HgId>,
        depth: Option<usize>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<Result<TreeEntry, EdenApiServerError>>, EdenApiError>;

    async fn commit_revlog_data(
        &self,
        repo: String,
        hgids: Vec<HgId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitRevlogData>, EdenApiError>;

    async fn clone_data(
        &self,
        repo: String,
        progress: Option<ProgressCallback>,
    ) -> Result<CloneData<HgId>, EdenApiError>;

    async fn full_idmap_clone_data(
        &self,
        repo: String,
        progress: Option<ProgressCallback>,
    ) -> Result<CloneData<HgId>, EdenApiError>;

    async fn commit_location_to_hash(
        &self,
        repo: String,
        requests: Vec<CommitLocationToHashRequest>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitLocationToHashResponse>, EdenApiError>;

    async fn commit_hash_to_location(
        &self,
        repo: String,
        master_heads: Vec<HgId>,
        hgids: Vec<HgId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitHashToLocationResponse>, EdenApiError>;

    /// Return a subset of commits that are known by the server.
    /// This is similar to the "known" command in HG wireproto.
    async fn commit_known(
        &self,
        repo: String,
        hgids: Vec<HgId>,
    ) -> Result<Fetch<CommitKnownResponse>, EdenApiError>;

    /// Return part of the commit graph that are ancestors of `heads`,
    /// excluding ancestors of `common`.
    ///
    /// This is `heads % common` (or `only(heads, common)`) expressed using HG
    /// revset, or `changelog.findmissing` in HG Python code base.
    ///
    /// If any of the nodes in `heads` and `common` are unknown by the server,
    /// it should be an error.
    async fn commit_graph(
        &self,
        repo: String,
        heads: Vec<HgId>,
        common: Vec<HgId>,
    ) -> Result<Fetch<CommitGraphEntry>, EdenApiError>;

    async fn bookmarks(
        &self,
        repo: String,
        bookmarks: Vec<String>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<BookmarkEntry>, EdenApiError>;

    /// Lookup items and return signed upload tokens if an item has been uploaded
    /// Supports: file content, hg filenode, hg tree, hg changeset
    async fn lookup_batch(
        &self,
        repo: String,
        items: Vec<AnyId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<LookupResponse>, EdenApiError>;
}

#[async_trait]
impl EdenApi for Arc<dyn EdenApi> {
    async fn health(&self) -> Result<ResponseMeta, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .health()
            .await
    }

    async fn files(
        &self,
        repo: String,
        keys: Vec<Key>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<FileEntry>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .files(repo, keys, progress)
            .await
    }

    async fn history(
        &self,
        repo: String,
        keys: Vec<Key>,
        length: Option<u32>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<HistoryEntry>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .history(repo, keys, length, progress)
            .await
    }

    async fn trees(
        &self,
        repo: String,
        keys: Vec<Key>,
        attributes: Option<TreeAttributes>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<Result<TreeEntry, EdenApiServerError>>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .trees(repo, keys, attributes, progress)
            .await
    }

    async fn complete_trees(
        &self,
        repo: String,
        rootdir: RepoPathBuf,
        mfnodes: Vec<HgId>,
        basemfnodes: Vec<HgId>,
        depth: Option<usize>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<Result<TreeEntry, EdenApiServerError>>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .complete_trees(repo, rootdir, mfnodes, basemfnodes, depth, progress)
            .await
    }

    async fn commit_revlog_data(
        &self,
        repo: String,
        hgids: Vec<HgId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitRevlogData>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .commit_revlog_data(repo, hgids, progress)
            .await
    }

    async fn clone_data(
        &self,
        repo: String,
        progress: Option<ProgressCallback>,
    ) -> Result<CloneData<HgId>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .clone_data(repo, progress)
            .await
    }

    async fn full_idmap_clone_data(
        &self,
        repo: String,
        progress: Option<ProgressCallback>,
    ) -> Result<CloneData<HgId>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .full_idmap_clone_data(repo, progress)
            .await
    }

    async fn commit_location_to_hash(
        &self,
        repo: String,
        requests: Vec<CommitLocationToHashRequest>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitLocationToHashResponse>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .commit_location_to_hash(repo, requests, progress)
            .await
    }

    async fn commit_hash_to_location(
        &self,
        repo: String,
        master_heads: Vec<HgId>,
        hgids: Vec<HgId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<CommitHashToLocationResponse>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .commit_hash_to_location(repo, master_heads, hgids, progress)
            .await
    }

    async fn commit_known(
        &self,
        repo: String,
        hgids: Vec<HgId>,
    ) -> Result<Fetch<CommitKnownResponse>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .commit_known(repo, hgids)
            .await
    }

    async fn commit_graph(
        &self,
        repo: String,
        heads: Vec<HgId>,
        common: Vec<HgId>,
    ) -> Result<Fetch<CommitGraphEntry>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .commit_graph(repo, heads, common)
            .await
    }

    async fn bookmarks(
        &self,
        repo: String,
        bookmarks: Vec<String>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<BookmarkEntry>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .bookmarks(repo, bookmarks, progress)
            .await
    }

    async fn lookup_batch(
        &self,
        repo: String,
        items: Vec<AnyId>,
        progress: Option<ProgressCallback>,
    ) -> Result<Fetch<LookupResponse>, EdenApiError> {
        <Arc<dyn EdenApi> as Borrow<dyn EdenApi>>::borrow(self)
            .lookup_batch(repo, items, progress)
            .await
    }
}
