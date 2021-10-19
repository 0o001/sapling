/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use anyhow::Result;
use async_runtime::block_on;
use futures::prelude::*;
use progress::Unit;
use tracing::field;

use super::hgid_keys;
use super::EdenApiRemoteStore;
use super::EdenApiStoreKind;
use super::File;
use super::Tree;
use crate::datastore::HgIdDataStore;
use crate::datastore::HgIdMutableDeltaStore;
use crate::datastore::Metadata;
use crate::datastore::RemoteDataStore;
use crate::datastore::StoreResult;
use crate::localstore::LocalStore;
use crate::types::StoreKey;
use crate::util;

/// A data store backed by an `EdenApiRemoteStore` and a mutable store.
///
/// Data will be fetched over the network via the remote store and stored in the
/// mutable store before being returned to the caller. This type is not exported
/// because it is intended to be used as a trait object.
pub(super) struct EdenApiDataStore<T> {
    remote: Arc<EdenApiRemoteStore<T>>,
    store: Arc<dyn HgIdMutableDeltaStore>,
}

impl<T: EdenApiStoreKind> EdenApiDataStore<T> {
    pub(super) fn new(
        remote: Arc<EdenApiRemoteStore<T>>,
        store: Arc<dyn HgIdMutableDeltaStore>,
    ) -> Self {
        Self { remote, store }
    }
}

impl RemoteDataStore for EdenApiDataStore<File> {
    fn prefetch(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        let client = self.remote.client.clone();
        let repo = self.remote.repo.clone();
        let progress = self.remote.progress.clone();
        let hgidkeys = hgid_keys(keys);

        let response = async move {
            let prog = progress.bar(
                "Downloading files over HTTP",
                Some(hgidkeys.len() as u64),
                Unit::Named("files"),
            )?;

            let mut response = File::prefetch_files(client, repo, hgidkeys).await?;
            while let Some(entry) = response.entries.try_next().await? {
                self.store.add_file(&entry)?;
                prog.increment(1)?;
            }
            // Explicitly force the result type here, since otherwise it can't infer the error
            // type.
            let result: Result<_> = Ok((self.store.get_missing(keys)?, response.stats.await?));
            result
        };

        let span = tracing::info_span!(
            "fetch_edenapi",
            downloaded = field::Empty,
            uploaded = field::Empty,
            requests = field::Empty,
            time = field::Empty,
            latency = field::Empty,
            download_speed = field::Empty,
            scmstore = false,
        );
        let _enter = span.enter();
        let (keys, stats) = block_on(response)?;
        util::record_edenapi_stats(&span, &stats);
        Ok(keys)
    }

    fn upload(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        // XXX: EdenAPI does not presently support uploads.
        Ok(keys.to_vec())
    }
}

impl RemoteDataStore for EdenApiDataStore<Tree> {
    fn prefetch(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        let client = self.remote.client.clone();
        let repo = self.remote.repo.clone();
        let progress = self.remote.progress.clone();
        let hgidkeys = hgid_keys(keys);

        let response = async move {
            let prog = progress.bar(
                "Downloading trees over HTTP",
                Some(hgidkeys.len() as u64),
                Unit::Named("trees"),
            )?;

            let mut response = Tree::prefetch_trees(client, repo, hgidkeys, None).await?;
            while let Some(Ok(entry)) = response.entries.try_next().await? {
                self.store.add_tree(&entry)?;
                prog.increment(1)?;
            }
            // Explicitly force the result type here, since otherwise it can't infer the error
            // type.
            let result: Result<_> = Ok((self.store.get_missing(keys)?, response.stats.await?));
            result
        };

        let span = tracing::info_span!(
            "fetch_edenapi",
            downloaded = field::Empty,
            uploaded = field::Empty,
            requests = field::Empty,
            time = field::Empty,
            latency = field::Empty,
            download_speed = field::Empty,
            scmstore = false,
        );
        let _enter = span.enter();
        let (keys, stats) = block_on(response)?;
        util::record_edenapi_stats(&span, &stats);
        Ok(keys)
    }

    fn upload(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        // XXX: EdenAPI does not presently support uploads.
        Ok(keys.to_vec())
    }
}

impl HgIdDataStore for EdenApiDataStore<File> {
    fn get(&self, key: StoreKey) -> Result<StoreResult<Vec<u8>>> {
        self.prefetch(&[key.clone()])?;
        self.store.get(key)
    }

    fn get_meta(&self, key: StoreKey) -> Result<StoreResult<Metadata>> {
        self.prefetch(&[key.clone()])?;
        self.store.get_meta(key)
    }

    fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

impl HgIdDataStore for EdenApiDataStore<Tree> {
    fn get(&self, key: StoreKey) -> Result<StoreResult<Vec<u8>>> {
        self.prefetch(&[key.clone()])?;
        self.store.get(key)
    }

    fn get_meta(&self, key: StoreKey) -> Result<StoreResult<Metadata>> {
        self.prefetch(&[key.clone()])?;
        self.store.get_meta(key)
    }

    fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

impl<T: EdenApiStoreKind> LocalStore for EdenApiDataStore<T> {
    fn get_missing(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        self.store.get_missing(keys)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use configparser::config::ConfigSet;
    use edenapi_types::ContentId;
    use edenapi_types::Sha1;
    use maplit::hashmap;
    use tempfile::TempDir;
    use types::testutil::*;
    use types::Sha256;

    use super::*;
    use crate::edenapi::File;
    use crate::edenapi::Tree;
    use crate::indexedlogauxstore::AuxStore;
    use crate::indexedlogdatastore::IndexedLogHgIdDataStore;
    use crate::indexedlogutil::StoreType;
    use crate::localstore::ExtStoredPolicy;
    use crate::scmstore::FileAttributes;
    use crate::scmstore::FileAuxData;
    use crate::scmstore::FileStore;
    use crate::scmstore::TreeStore;
    use crate::testutil::*;

    #[test]
    fn test_get_file() -> Result<()> {
        // Set up mocked EdenAPI file and tree stores.
        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");
        let d = delta("1234", None, k.clone());
        let files = hashmap! { k.clone() => d.data.clone() };

        let client = FakeEdenApi::new().files(files).into_arc();
        let remote_files = EdenApiRemoteStore::<File>::new("repo", client, None);

        // Set up local cache store to write received data.
        let mut store = FileStore::empty();

        let tmp = TempDir::new()?;
        let cache = Arc::new(IndexedLogHgIdDataStore::new(
            &tmp,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?);
        store.indexedlog_cache = Some(cache.clone());
        store.edenapi = Some(remote_files);

        // Attempt fetch.
        let mut fetched = store
            .fetch(std::iter::once(k.clone()), FileAttributes::CONTENT)
            .single()?
            .expect("key not found");
        assert_eq!(fetched.file_content()?.to_vec(), d.data.as_ref().to_vec());

        // Check that data was written to the local store.
        let mut fetched = cache.get_entry(k.clone())?.expect("key not found");
        assert_eq!(fetched.content()?.to_vec(), d.data.as_ref().to_vec());

        Ok(())
    }

    #[test]
    fn test_get_tree() -> Result<()> {
        // Set up mocked EdenAPI file and tree stores.
        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");
        let d = delta("1234", None, k.clone());
        let trees = hashmap! { k.clone() => d.data.clone() };

        let client = FakeEdenApi::new().trees(trees).into_arc();
        let remote_trees = EdenApiRemoteStore::<Tree>::new("repo", client, None);

        // Set up local cache store to write received data.
        let mut store = TreeStore::empty();

        let tmp = TempDir::new()?;
        let cache = Arc::new(IndexedLogHgIdDataStore::new(
            &tmp,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?);
        store.indexedlog_cache = Some(cache.clone());
        store.edenapi = Some(remote_trees);

        // Attempt fetch.
        let mut fetched = store
            .fetch_batch(std::iter::once(k.clone()))?
            .single()?
            .expect("key not found");
        assert_eq!(
            fetched.manifest_tree_entry()?.0.to_vec(),
            d.data.as_ref().to_vec()
        );

        // Check that data was written to the local store.
        let mut fetched = cache.get_entry(k.clone())?.expect("key not found");
        assert_eq!(fetched.content()?.to_vec(), d.data.as_ref().to_vec());

        Ok(())
    }

    #[test]
    fn test_not_found() -> Result<()> {
        let client = FakeEdenApi::new().into_arc();
        let remote_trees = EdenApiRemoteStore::<Tree>::new("repo", client, None);

        // Set up local cache store to write received data.
        let mut store = TreeStore::empty();
        store.edenapi = Some(remote_trees);

        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");

        // Attempt fetch.
        let fetched = store.fetch_batch(std::iter::once(k.clone()))?;
        assert_eq!(fetched.complete.len(), 0);
        assert_eq!(fetched.incomplete.into_keys().collect::<Vec<_>>(), vec![k]);

        Ok(())
    }

    #[test]
    fn test_get_aux_cache() -> Result<()> {
        // Set up mocked EdenAPI file and tree stores.
        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");
        let d = delta("1234", None, k.clone());
        let files = hashmap! { k.clone() => d.data.clone() };

        let client = FakeEdenApi::new().files(files).into_arc();
        let remote_files = EdenApiRemoteStore::<File>::new("repo", client, None);

        // Set up local cache store to write received data.
        let mut store = FileStore::empty();
        store.edenapi = Some(remote_files);

        // Empty aux cache
        let tmp = TempDir::new()?;
        let aux_cache = Arc::new(AuxStore::new(&tmp, &ConfigSet::new(), StoreType::Shared)?);
        store.aux_cache = Some(aux_cache.clone());

        // Empty content cache
        let tmp = TempDir::new()?;
        let cache = Arc::new(IndexedLogHgIdDataStore::new(
            &tmp,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?);
        store.indexedlog_cache = Some(cache.clone());

        store.cache_to_local_cache = false;

        let expected = FileAuxData {
            total_size: 4,
            content_id: ContentId::from_str(
                "aa6ab85da77ca480b7624172fe44aa9906b6c3f00f06ff23c3e5f60bfd0c414e",
            )?,
            content_sha1: Sha1::from_str("7110eda4d09e062aa5e4a390b0a572ac0d2c0220")?,
            content_sha256: Sha256::from_str(
                "03ac674216f3e15c761ee1a5e255f067953623c8b388b4459e13f978d7c846f4",
            )?,
        };

        // Test that we can read aux data from EdenApi
        let fetched = store
            .fetch(std::iter::once(k.clone()), FileAttributes::AUX)
            .single()?
            .expect("key not found");
        assert_eq!(fetched.aux_data().expect("no aux data found"), expected);

        // Disable EdenApi and local cache, make sure we can read from aux cache.
        store.edenapi = None;
        store.indexedlog_cache = None;
        let fetched = store
            .fetch(std::iter::once(k.clone()), FileAttributes::AUX)
            .single()?
            .expect("key not found");
        assert_eq!(fetched.aux_data().expect("no aux data found"), expected);


        // Content shouldn't have been cached
        assert_eq!(cache.get_entry(k.clone())?, None);

        Ok(())
    }
}
