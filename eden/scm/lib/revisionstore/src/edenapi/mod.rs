/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;

use edenapi::{BlockingResponse, EdenApi, EdenApiBlocking, EdenApiError, Response};
use edenapi_types::{EdenApiServerError, FileEntry, FileSpec, TreeAttributes, TreeEntry};
use progress::{NullProgressFactory, ProgressFactory};
use types::Key;

use crate::{
    datastore::{HgIdMutableDeltaStore, RemoteDataStore},
    historystore::{HgIdMutableHistoryStore, RemoteHistoryStore},
    remotestore::HgIdRemoteStore,
    types::StoreKey,
};

mod data;
mod history;

use data::EdenApiDataStore;
use history::EdenApiHistoryStore;

/// Convenience aliases for file and tree stores.
pub type EdenApiFileStore = EdenApiRemoteStore<File>;
pub type EdenApiTreeStore = EdenApiRemoteStore<Tree>;

/// A shim around an EdenAPI client that implements the various traits of
/// Mercurial's storage layer, allowing a type that implements `EdenApi` to be
/// used alongside other Mercurial data and history stores.
///
/// Note that this struct does not allow for data fetching on its own, because
/// it does not contain a mutable store into which to write the fetched data.
/// Use the methods from the `HgIdRemoteStore` trait to provide an appropriate
/// mutable store.
#[derive(Clone)]
pub struct EdenApiRemoteStore<T> {
    client: Arc<dyn EdenApi>,
    repo: String,
    progress: Arc<dyn ProgressFactory>,
    _phantom: PhantomData<T>,
}

impl<T: EdenApiStoreKind> EdenApiRemoteStore<T> {
    /// Create a new EdenApiRemoteStore using the given EdenAPI client.
    ///
    /// In the current design of Mercurial's data storage layer, stores are
    /// typically tied to a particular repo. The `EdenApi` trait itself is
    /// repo-agnostic and requires the caller to specify the desired repo. As
    /// a result, an `EdenApiStore` needs to be passed the name of the repo
    /// it belongs to so it can pass it to the underlying EdenAPI client.edenapi
    ///
    /// The current design of the storage layer also requires a distinction
    /// between stores that provide file data and stores that provide tree data.
    /// (This is because both kinds of data are fetched via the `prefetch()`
    /// method from the `RemoteDataStore` trait.)
    ///
    /// The kind of data fetched by a store can be specified via a marker type;
    /// in particular, `File` or `Tree`. For example, a store that fetches file
    /// data would be created as follows:
    ///
    /// ```rust,ignore
    /// let store = EdenApiStore::<File>::new(repo, edenapi);
    /// ```
    pub fn new(
        repo: impl ToString,
        client: Arc<dyn EdenApi>,
        progress: Option<Arc<dyn ProgressFactory>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            repo: repo.to_string(),
            progress: progress.unwrap_or_else(|| NullProgressFactory::arc()),
            _phantom: PhantomData,
        })
    }
}

impl HgIdRemoteStore for EdenApiRemoteStore<File> {
    fn datastore(
        self: Arc<Self>,
        store: Arc<dyn HgIdMutableDeltaStore>,
    ) -> Arc<dyn RemoteDataStore> {
        Arc::new(EdenApiDataStore::new(self, store))
    }

    fn historystore(
        self: Arc<Self>,
        store: Arc<dyn HgIdMutableHistoryStore>,
    ) -> Arc<dyn RemoteHistoryStore> {
        Arc::new(EdenApiHistoryStore::new(self, store))
    }
}

impl HgIdRemoteStore for EdenApiRemoteStore<Tree> {
    fn datastore(
        self: Arc<Self>,
        store: Arc<dyn HgIdMutableDeltaStore>,
    ) -> Arc<dyn RemoteDataStore> {
        Arc::new(EdenApiDataStore::new(self, store))
    }

    fn historystore(
        self: Arc<Self>,
        _store: Arc<dyn HgIdMutableHistoryStore>,
    ) -> Arc<dyn RemoteHistoryStore> {
        unimplemented!("EdenAPI does not support fetching tree history")
    }
}

/// Marker type indicating that the store fetches file data.
pub enum File {}

/// Marker type indicating that the store fetches tree data.
pub enum Tree {}

impl EdenApiFileStore {
    pub fn files_blocking(
        &self,
        keys: Vec<Key>,
    ) -> Result<BlockingResponse<FileEntry>, EdenApiError> {
        self.client.files_blocking(self.repo.clone(), keys)
    }

    pub fn files_attrs_blocking(
        &self,
        reqs: Vec<FileSpec>,
    ) -> Result<BlockingResponse<FileEntry>, EdenApiError> {
        self.client.files_attrs_blocking(self.repo.clone(), reqs)
    }
}

impl EdenApiTreeStore {
    pub fn trees_blocking(
        &self,
        keys: Vec<Key>,
        attributes: Option<TreeAttributes>,
    ) -> Result<BlockingResponse<Result<TreeEntry, EdenApiServerError>>, EdenApiError> {
        self.client
            .trees_blocking(self.repo.clone(), keys, attributes)
    }
}

/// Trait that provides a common interface for calling the `files` and `trees`
/// methods on an EdenAPI client.
#[async_trait]
pub trait EdenApiStoreKind: Send + Sync + 'static {
    async fn prefetch_files(
        _client: Arc<dyn EdenApi>,
        _repo: String,
        _keys: Vec<Key>,
    ) -> Result<Response<FileEntry>, EdenApiError> {
        unimplemented!("fetching files not supported for this store")
    }

    async fn prefetch_trees(
        _client: Arc<dyn EdenApi>,
        _repo: String,
        _keys: Vec<Key>,
        _attributes: Option<TreeAttributes>,
    ) -> Result<Response<Result<TreeEntry, EdenApiServerError>>, EdenApiError> {
        unimplemented!("fetching trees not supported for this store")
    }
}

#[async_trait]
impl EdenApiStoreKind for File {
    async fn prefetch_files(
        client: Arc<dyn EdenApi>,
        repo: String,
        keys: Vec<Key>,
    ) -> Result<Response<FileEntry>, EdenApiError> {
        client.files(repo, keys).await
    }
}

#[async_trait]
impl EdenApiStoreKind for Tree {
    async fn prefetch_trees(
        client: Arc<dyn EdenApi>,
        repo: String,
        keys: Vec<Key>,
        attributes: Option<TreeAttributes>,
    ) -> Result<Response<Result<TreeEntry, EdenApiServerError>>, EdenApiError> {
        client.trees(repo, keys, attributes).await
    }
}

/// Return only the HgId keys from the given iterator.
/// EdenAPI cannot fetch content-addressed LFS blobs.
fn hgid_keys<'a>(keys: impl IntoIterator<Item = &'a StoreKey>) -> Vec<Key> {
    keys.into_iter()
        .filter_map(|k| match k {
            StoreKey::HgId(k) => Some(k.clone()),
            StoreKey::Content(..) => None,
        })
        .collect()
}
