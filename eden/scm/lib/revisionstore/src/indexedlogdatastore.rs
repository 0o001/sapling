/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Result};
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use bytes::Bytes;
use parking_lot::RwLock;

use configparser::{
    config::ConfigSet,
    hg::{ByteCount, ConfigSetHgExt},
};
use indexedlog::log::IndexOutput;
use lz4_pyframe::{compress, decompress};
use types::{hgid::ReadHgIdExt, HgId, Key, RepoPath};

use crate::{
    datastore::{Delta, HgIdDataStore, HgIdMutableDeltaStore, Metadata, StoreResult},
    indexedlogutil::{Store, StoreOpenOptions},
    localstore::{ExtStoredPolicy, LocalStore},
    repack::ToKeys,
    sliceext::SliceExt,
    types::StoreKey,
};

#[derive(Clone, Copy)]
pub enum IndexedLogDataStoreType {
    Local,
    Shared,
}

struct IndexedLogHgIdDataStoreInner {
    log: Store,
}

pub struct IndexedLogHgIdDataStore {
    inner: RwLock<IndexedLogHgIdDataStoreInner>,
    extstored_policy: ExtStoredPolicy,
}

struct Entry {
    key: Key,
    metadata: Metadata,

    content: Option<Bytes>,
    compressed_content: Option<Bytes>,
}

impl Entry {
    pub fn new(key: Key, content: Bytes, metadata: Metadata) -> Self {
        Entry {
            key,
            content: Some(content),
            metadata,
            compressed_content: None,
        }
    }

    /// Read an entry from the slice and deserialize it.
    ///
    /// The on-disk format of an entry is the following:
    /// - HgId <20 bytes>
    /// - Path len: 2 unsigned bytes, big-endian
    /// - Path: <Path len> bytes
    /// - Metadata: metadata-list
    /// - Content len: 8 unsigned bytes, big-endian
    /// - Content: <Content len> bytes, lz4 compressed
    ///
    /// The metadata-list is a list of Metadata, encode with:
    /// - Flag: 1 byte,
    /// - Len: 2 unsigned bytes, big-endian
    /// - Value: <Len> bytes, big-endian
    fn from_slice(data: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(data);
        let hgid = cur.read_hgid()?;

        let name_len = cur.read_u16::<BigEndian>()? as u64;
        let name_slice =
            data.get_err(cur.position() as usize..(cur.position() + name_len) as usize)?;
        cur.set_position(cur.position() + name_len);
        let filename = RepoPath::from_utf8(name_slice)?;

        let key = Key::new(filename.to_owned(), hgid);

        let metadata = Metadata::read(&mut cur)?;

        let compressed_len = cur.read_u64::<BigEndian>()?;
        let compressed =
            data.get_err(cur.position() as usize..(cur.position() + compressed_len) as usize)?;

        Ok(Entry {
            key,
            content: None,
            compressed_content: Some(Bytes::copy_from_slice(compressed)),
            metadata,
        })
    }

    /// Read an entry from the IndexedLog and deserialize it.
    pub fn from_log(key: &Key, log: &Store) -> Result<Option<Self>> {
        let mut log_entry = log.lookup(0, key.hgid.as_ref().to_vec())?;
        let buf = match log_entry.next() {
            None => return Ok(None),
            Some(buf) => buf?,
        };

        Entry::from_slice(buf).map(Some)
    }

    /// Write an entry to the IndexedLog. See [`from_log`] for the detail about the on-disk format.
    pub fn write_to_log(self, log: &mut Store) -> Result<()> {
        let mut buf = Vec::new();
        buf.write_all(self.key.hgid.as_ref())?;
        let path_slice = self.key.path.as_byte_slice();
        buf.write_u16::<BigEndian>(path_slice.len() as u16)?;
        buf.write_all(path_slice)?;
        self.metadata.write(&mut buf)?;

        let compressed = if let Some(compressed) = self.compressed_content {
            compressed
        } else {
            if let Some(raw) = self.content {
                compress(&raw)?.into()
            } else {
                bail!("No content");
            }
        };

        buf.write_u64::<BigEndian>(compressed.len() as u64)?;
        buf.write_all(&compressed)?;

        Ok(log.append(buf)?)
    }

    pub fn content(&mut self) -> Result<Bytes> {
        if let Some(content) = self.content.as_ref() {
            return Ok(content.clone());
        }

        if let Some(compressed) = self.compressed_content.as_ref() {
            let raw = Bytes::from(decompress(&compressed)?);
            self.content = Some(raw.clone());
            Ok(raw)
        } else {
            bail!("No content");
        }
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

impl IndexedLogHgIdDataStore {
    /// Create or open an `IndexedLogHgIdDataStore`.
    pub fn new(
        path: impl AsRef<Path>,
        extstored_policy: ExtStoredPolicy,
        config: &ConfigSet,
        store_type: IndexedLogDataStoreType,
    ) -> Result<Self> {
        let open_options = IndexedLogHgIdDataStore::open_options(config)?;

        let log = match store_type {
            IndexedLogDataStoreType::Local => open_options.local(&path),
            IndexedLogDataStoreType::Shared => open_options.shared(&path),
        }?;

        Ok(IndexedLogHgIdDataStore {
            inner: RwLock::new(IndexedLogHgIdDataStoreInner { log }),
            extstored_policy,
        })
    }

    fn open_options(config: &ConfigSet) -> Result<StoreOpenOptions> {
        // Default configuration: 4 x 2.5GB.
        let mut open_options = StoreOpenOptions::new()
            .max_log_count(4)
            .max_bytes_per_log(2500 * 1000 * 1000)
            .auto_sync_threshold(250 * 1024 * 1024)
            .create(true)
            .index("node", |_| {
                vec![IndexOutput::Reference(0..HgId::len() as u64)]
            });

        if let Some(max_log_count) = config.get_opt::<u8>("indexedlog", "data.max-log-count")? {
            open_options = open_options.max_log_count(max_log_count);
        }
        if let Some(max_bytes_per_log) =
            config.get_opt::<ByteCount>("indexedlog", "data.max-bytes-per-log")?
        {
            open_options = open_options.max_bytes_per_log(max_bytes_per_log.value());
        } else if let Some(max_bytes_per_log) =
            config.get_opt::<ByteCount>("remotefilelog", "cachelimit")?
        {
            let log_count: u64 = open_options.max_log_count.unwrap_or(1).max(1).into();
            open_options =
                open_options.max_bytes_per_log((max_bytes_per_log.value() / log_count).max(1));
        }
        Ok(open_options)
    }

    pub fn repair(
        path: PathBuf,
        config: &ConfigSet,
        store_type: IndexedLogDataStoreType,
    ) -> Result<String> {
        match store_type {
            IndexedLogDataStoreType::Local => {
                IndexedLogHgIdDataStore::open_options(config)?.repair_local(path)
            }
            IndexedLogDataStoreType::Shared => {
                IndexedLogHgIdDataStore::open_options(config)?.repair_shared(path)
            }
        }
    }
}

impl HgIdMutableDeltaStore for IndexedLogHgIdDataStore {
    fn add(&self, delta: &Delta, metadata: &Metadata) -> Result<()> {
        ensure!(delta.base.is_none(), "Deltas aren't supported.");

        let entry = Entry::new(delta.key.clone(), delta.data.clone(), metadata.clone());
        let mut inner = self.inner.write();
        entry.write_to_log(&mut inner.log)
    }

    fn flush(&self) -> Result<Option<Vec<PathBuf>>> {
        self.inner.write().log.flush()?;
        Ok(None)
    }
}

impl LocalStore for IndexedLogHgIdDataStore {
    fn get_missing(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        let inner = self.inner.read();
        Ok(keys
            .iter()
            .filter(|k| match k {
                StoreKey::HgId(k) => match Entry::from_log(k, &inner.log) {
                    Ok(None) | Err(_) => true,
                    Ok(Some(_)) => false,
                },
                StoreKey::Content(_, _) => true,
            })
            .cloned()
            .collect())
    }
}

impl HgIdDataStore for IndexedLogHgIdDataStore {
    fn get(&self, key: StoreKey) -> Result<StoreResult<Vec<u8>>> {
        let key = match key {
            StoreKey::HgId(key) => key,
            content => return Ok(StoreResult::NotFound(content)),
        };

        let inner = self.inner.read();
        let mut entry = match Entry::from_log(&key, &inner.log)? {
            None => return Ok(StoreResult::NotFound(StoreKey::HgId(key))),
            Some(entry) => entry,
        };

        if self.extstored_policy == ExtStoredPolicy::Ignore && entry.metadata().is_lfs() {
            Ok(StoreResult::NotFound(StoreKey::HgId(key)))
        } else {
            let content = entry.content()?;
            Ok(StoreResult::Found(content.as_ref().to_vec()))
        }
    }

    fn get_meta(&self, key: StoreKey) -> Result<StoreResult<Metadata>> {
        let key = match key {
            StoreKey::HgId(key) => key,
            content => return Ok(StoreResult::NotFound(content)),
        };

        let inner = self.inner.read();
        let entry = match Entry::from_log(&key, &inner.log)? {
            None => return Ok(StoreResult::NotFound(StoreKey::HgId(key))),
            Some(entry) => entry,
        };

        let metadata = entry.metadata();
        if self.extstored_policy == ExtStoredPolicy::Ignore && entry.metadata().is_lfs() {
            Ok(StoreResult::NotFound(StoreKey::HgId(key)))
        } else {
            Ok(StoreResult::Found(metadata.clone()))
        }
    }

    fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

impl ToKeys for IndexedLogHgIdDataStore {
    fn to_keys(&self) -> Vec<Result<Key>> {
        self.inner
            .read()
            .log
            .iter()
            .map(|entry| Entry::from_slice(entry?))
            .map(|entry| Ok(entry?.key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::remove_file;

    use bytes::Bytes;
    use tempfile::TempDir;

    use types::testutil::*;

    #[test]
    fn test_empty() {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();
        log.flush().unwrap();
    }

    #[test]
    fn test_add() {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();

        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: key("a", "1"),
        };
        let metadata = Default::default();

        log.add(&delta, &metadata).unwrap();
        log.flush().unwrap();
    }

    #[test]
    fn test_add_get() {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();

        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: key("a", "1"),
        };
        let metadata = Default::default();

        log.add(&delta, &metadata).unwrap();
        log.flush().unwrap();

        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();
        let read_data = log.get(StoreKey::hgid(delta.key)).unwrap();
        assert_eq!(StoreResult::Found(delta.data.as_ref().to_vec()), read_data);
    }

    #[test]
    fn test_lookup_failure() {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();

        let key = StoreKey::hgid(key("a", "1"));
        assert_eq!(log.get(key.clone()).unwrap(), StoreResult::NotFound(key));
    }

    #[test]
    fn test_add_chain() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )?;

        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: Some(key("a", "1")),
            key: key("a", "2"),
        };
        let metadata = Default::default();

        assert!(log.add(&delta, &metadata).is_err());
        Ok(())
    }

    #[test]
    fn test_iter() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )?;

        let k = key("a", "2");
        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: k.clone(),
        };
        let metadata = Default::default();

        log.add(&delta, &metadata)?;
        assert!(log.to_keys().into_iter().all(|e| e.unwrap() == k));
        Ok(())
    }

    #[test]
    fn test_corrupted() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )?;

        let k = key("a", "2");
        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: k.clone(),
        };
        let metadata = Default::default();

        log.add(&delta, &metadata)?;
        log.flush()?;
        drop(log);

        // Corrupt the log by removing the "log" file.
        let mut rotate_log_path = tempdir.path().to_path_buf();
        rotate_log_path.push("0");
        rotate_log_path.push("log");
        remove_file(rotate_log_path)?;

        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )?;
        let k = key("a", "3");
        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: k.clone(),
        };
        let metadata = Default::default();
        log.add(&delta, &metadata)?;
        log.flush()?;

        // There should be only one key in the store.
        assert_eq!(log.to_keys().into_iter().count(), 1);
        Ok(())
    }

    #[test]
    fn test_extstored_ignore() -> Result<()> {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();

        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: key("a", "1"),
        };

        log.add(
            &delta,
            &Metadata {
                size: None,
                flags: Some(Metadata::LFS_FLAG),
            },
        )?;

        let k = StoreKey::hgid(delta.key);
        assert_eq!(log.get(k.clone())?, StoreResult::NotFound(k));

        Ok(())
    }

    #[test]
    fn test_extstored_use() -> Result<()> {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            IndexedLogDataStoreType::Shared,
        )
        .unwrap();

        let delta = Delta {
            data: Bytes::from(&[1, 2, 3, 4][..]),
            base: None,
            key: key("a", "1"),
        };

        log.add(
            &delta,
            &Metadata {
                size: None,
                flags: Some(Metadata::LFS_FLAG),
            },
        )?;

        let k = StoreKey::hgid(delta.key);
        assert_eq!(
            log.get(k)?,
            StoreResult::Found(delta.data.as_ref().to_vec())
        );

        Ok(())
    }
}
