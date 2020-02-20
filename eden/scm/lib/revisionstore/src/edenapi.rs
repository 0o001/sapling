/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use anyhow::Result;

use edenapi::EdenApi;
use types::Key;

use crate::{
    datastore::{DataStore, Delta, Metadata, MutableDeltaStore, RemoteDataStore},
    historystore::{MutableHistoryStore, RemoteHistoryStore},
    localstore::LocalStore,
    remotestore::RemoteStore,
};

#[derive(Clone)]
enum EdenApiRemoteStoreKind {
    File,
    Tree,
}

/// Small shim around `EdenApi` that implements the `RemoteDataStore` and `DataStore` trait. All
/// the `DataStore` methods will always fetch data from the network.
#[derive(Clone)]
pub struct EdenApiRemoteStore {
    edenapi: Arc<dyn EdenApi>,
    kind: EdenApiRemoteStoreKind,
}

impl EdenApiRemoteStore {
    pub fn filestore(edenapi: Arc<dyn EdenApi>) -> Self {
        Self {
            edenapi,
            kind: EdenApiRemoteStoreKind::File,
        }
    }

    pub fn treestore(edenapi: Arc<dyn EdenApi>) -> Self {
        Self {
            edenapi,
            kind: EdenApiRemoteStoreKind::Tree,
        }
    }
}

impl RemoteStore for EdenApiRemoteStore {
    fn datastore(&self, store: Arc<dyn MutableDeltaStore>) -> Arc<dyn RemoteDataStore> {
        Arc::new(EdenApiRemoteDataStore {
            inner: EdenApiRemoteDataStoreInner {
                edenapi: self.clone(),
                store,
            },
        })
    }

    fn historystore(&self, _store: Arc<dyn MutableHistoryStore>) -> Arc<dyn RemoteHistoryStore> {
        unimplemented!()
    }
}

struct EdenApiRemoteDataStoreInner {
    edenapi: EdenApiRemoteStore,
    store: Arc<dyn MutableDeltaStore>,
}

struct EdenApiRemoteDataStore {
    inner: EdenApiRemoteDataStoreInner,
}

impl RemoteDataStore for EdenApiRemoteDataStore {
    fn prefetch(&self, keys: &[Key]) -> Result<()> {
        let edenapi = &self.inner.edenapi;
        let (entries, _) = match edenapi.kind {
            EdenApiRemoteStoreKind::File => edenapi.edenapi.get_files(keys.to_vec(), None)?,
            EdenApiRemoteStoreKind::Tree => edenapi.edenapi.get_trees(keys.to_vec(), None)?,
        };
        for entry in entries {
            let key = entry.0.clone();
            let data = entry.1;
            let metadata = Metadata {
                size: Some(data.len() as u64),
                flags: None,
            };
            let delta = Delta {
                data,
                base: None,
                key,
            };
            self.inner.store.add(&delta, &metadata)?;
        }
        Ok(())
    }
}

impl DataStore for EdenApiRemoteDataStore {
    fn get(&self, _key: &Key) -> Result<Option<Vec<u8>>> {
        unreachable!();
    }

    fn get_delta(&self, key: &Key) -> Result<Option<Delta>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self.inner.store.get_delta(key),
            Err(_) => Ok(None),
        }
    }

    fn get_delta_chain(&self, key: &Key) -> Result<Option<Vec<Delta>>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self.inner.store.get_delta_chain(key),
            Err(_) => Ok(None),
        }
    }

    fn get_meta(&self, key: &Key) -> Result<Option<Metadata>> {
        match self.prefetch(&[key.clone()]) {
            Ok(()) => self.inner.store.get_meta(key),
            Err(_) => Ok(None),
        }
    }
}

impl LocalStore for EdenApiRemoteDataStore {
    fn get_missing(&self, keys: &[Key]) -> Result<Vec<Key>> {
        Ok(keys.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use tempfile::TempDir;

    use types::testutil::*;

    use crate::{indexedlogdatastore::IndexedLogDataStore, testutil::*};

    #[test]
    fn test_get_delta() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = Arc::new(IndexedLogDataStore::new(&tmp)?);

        let k = key("a", "1");
        let d = delta("1234", None, k.clone());

        let mut map = HashMap::new();
        map.insert(k.clone(), d.data.clone());

        let edenapi = EdenApiRemoteStore::filestore(fake_edenapi(map));

        let remotestore = edenapi.datastore(store.clone());
        assert_eq!(remotestore.get_delta(&k)?.unwrap(), d);
        assert_eq!(store.get_delta(&k)?.unwrap(), d);

        Ok(())
    }

    #[test]
    fn test_missing() -> Result<()> {
        let tmp = TempDir::new()?;
        let store = Arc::new(IndexedLogDataStore::new(&tmp)?);

        let map = HashMap::new();
        let edenapi = EdenApiRemoteStore::filestore(fake_edenapi(map));

        let remotestore = edenapi.datastore(store);

        let k = key("a", "1");
        assert_eq!(remotestore.get_delta(&k)?, None);
        Ok(())
    }
}
