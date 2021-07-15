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
use minibytes::Bytes;
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use configparser::{config::ConfigSet, convert::ByteCount};
use edenapi_types::{FileEntry, TreeEntry};
use indexedlog::log::IndexOutput;
use lz4_pyframe::{compress, decompress};
use tracing::warn;
use types::{hgid::ReadHgIdExt, HgId, Key, RepoPath};

use crate::missing::MissingInjection;
use crate::{
    datastore::{Delta, HgIdDataStore, HgIdMutableDeltaStore, Metadata, StoreResult},
    indexedlogutil::{Store, StoreOpenOptions, StoreType},
    localstore::{ExtStoredPolicy, LocalStore},
    repack::ToKeys,
    sliceext::SliceExt,
    types::StoreKey,
};

struct IndexedLogHgIdDataStoreInner {
    pub log: Store,
}

pub struct IndexedLogHgIdDataStore {
    inner: RwLock<IndexedLogHgIdDataStoreInner>,
    extstored_policy: ExtStoredPolicy,
    missing: MissingInjection,
}

pub struct IndexedLogHgIdDataStoreReadGuard<'a>(RwLockReadGuard<'a, IndexedLogHgIdDataStoreInner>);

pub struct IndexedLogHgIdDataStoreWriteGuard<'a>(
    RwLockWriteGuard<'a, IndexedLogHgIdDataStoreInner>,
);

#[derive(Clone, Debug)]
pub struct Entry {
    key: Key,
    metadata: Metadata,

    content: Option<Bytes>,
    compressed_content: Option<Bytes>,
}

impl std::cmp::PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.metadata == other.metadata
            && match (self.content_inner(), other.content_inner()) {
                (Ok(c1), Ok(c2)) if c1 == c2 => true,
                _ => false,
            }
    }
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
    fn from_bytes(bytes: Bytes) -> Result<Self> {
        let data: &[u8] = bytes.as_ref();
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
        let bytes = bytes.slice_to_bytes(compressed);

        Ok(Entry {
            key,
            content: None,
            compressed_content: Some(bytes),
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

        let bytes = log.slice_to_bytes(buf);
        Entry::from_bytes(bytes).map(Some)
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

    fn content_inner(&self) -> Result<Bytes> {
        if let Some(content) = self.content.as_ref() {
            return Ok(content.clone());
        }

        if let Some(compressed) = self.compressed_content.as_ref() {
            let raw = Bytes::from(decompress(&compressed)?);
            Ok(raw)
        } else {
            bail!("No content");
        }
    }

    pub fn content(&mut self) -> Result<Bytes> {
        self.content = Some(self.content_inner()?);
        // this unwrap is safe because we assign the field in the line above
        Ok(self.content.as_ref().unwrap().clone())
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Replaces the Entry's key in case caller looked up a different path.
    pub(crate) fn with_key(self, key: Key) -> Self {
        Entry {
            key,
            content: self.content,
            metadata: self.metadata,
            compressed_content: self.compressed_content,
        }
    }
}

impl IndexedLogHgIdDataStore {
    /// Create or open an `IndexedLogHgIdDataStore`.
    pub fn new(
        path: impl AsRef<Path>,
        extstored_policy: ExtStoredPolicy,
        config: &ConfigSet,
        store_type: StoreType,
    ) -> Result<Self> {
        let open_options = IndexedLogHgIdDataStore::open_options(config)?;

        let log = match store_type {
            StoreType::Local => open_options.local(&path),
            StoreType::Shared => open_options.shared(&path),
        }?;

        Ok(IndexedLogHgIdDataStore {
            inner: RwLock::new(IndexedLogHgIdDataStoreInner { log }),
            extstored_policy,
            missing: MissingInjection::new_from_env("MISSING_FILES"),
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

    pub fn repair(path: PathBuf, config: &ConfigSet, store_type: StoreType) -> Result<String> {
        match store_type {
            StoreType::Local => IndexedLogHgIdDataStore::open_options(config)?.repair_local(path),
            StoreType::Shared => IndexedLogHgIdDataStore::open_options(config)?.repair_shared(path),
        }
    }

    /// Attempt to read an Entry from IndexedLog, replacing the stored path with the one from the provided Key
    pub fn get_entry(&self, key: Key) -> Result<Option<Entry>> {
        Ok(self.get_raw_entry(&key)?.map(|e| e.with_key(key)))
    }

    // TODO(meyer): Make IndexedLogHgIdDataStore "directly" lockable so we can lock and do a batch of operations (RwLock Guard pattern)
    /// Attempt to read an Entry from IndexedLog, without overwriting the Key (return Key path may not match the request Key path)
    pub(crate) fn get_raw_entry(&self, key: &Key) -> Result<Option<Entry>> {
        let inner = self.inner.read();
        Entry::from_log(key, &inner.log)
    }

    /// Write an entry to the IndexedLog
    pub fn put_entry(&self, entry: Entry) -> Result<()> {
        let mut inner = self.inner.write();
        entry.write_to_log(&mut inner.log)
    }

    /// Flush the underlying IndexedLog
    pub fn flush_log(&self) -> Result<()> {
        self.inner.write().log.flush()?;
        Ok(())
    }

    pub fn read_lock<'a>(&'a self) -> IndexedLogHgIdDataStoreReadGuard<'a> {
        IndexedLogHgIdDataStoreReadGuard(self.inner.read())
    }

    pub fn write_lock<'a>(&'a self) -> IndexedLogHgIdDataStoreWriteGuard<'a> {
        IndexedLogHgIdDataStoreWriteGuard(self.inner.write())
    }
}

impl<'a> IndexedLogHgIdDataStoreReadGuard<'a> {
    /// Write an entry to the IndexedLog
    ///
    /// Like IndexedLogHgIdDataStore::get_entry, but uses the already-acquired read lock.
    pub fn get_entry(&self, key: Key) -> Result<Option<Entry>> {
        Ok(self.get_raw_entry(&key)?.map(|e| e.with_key(key)))
    }

    /// Attempt to read an Entry from IndexedLog, without overwriting the Key (return Key path may not match the request Key path)
    ///
    /// Like IndexedLogHgIdDataStore::get_raw_entry, but uses the already-acquired read lock.
    pub(crate) fn get_raw_entry(&self, key: &Key) -> Result<Option<Entry>> {
        Entry::from_log(key, &self.0.log)
    }
}

impl<'a> IndexedLogHgIdDataStoreWriteGuard<'a> {
    /// Write an entry to the IndexedLog
    ///
    /// Like IndexedLogHgIdDataStore::get_entry, but uses the already-acquired write lock.
    pub fn get_entry(&self, key: Key) -> Result<Option<Entry>> {
        Ok(self.get_raw_entry(&key)?.map(|e| e.with_key(key)))
    }

    /// Attempt to read an Entry from IndexedLog, without overwriting the Key (return Key path may not match the request Key path)
    ///
    /// Like IndexedLogHgIdDataStore::get_raw_entry, but uses the already-acquired write lock.
    pub(crate) fn get_raw_entry(&self, key: &Key) -> Result<Option<Entry>> {
        Entry::from_log(key, &self.0.log)
    }

    /// Write an entry to the IndexedLog
    ///
    /// Like IndexedLogHgIdDataStore::put_entry, but uses the already-acquired write lock.
    pub fn put_entry(&mut self, entry: Entry) -> Result<()> {
        entry.write_to_log(&mut self.0.log)
    }

    /// Flush the underlying IndexedLog
    ///
    /// Like IndexedLogHgIdDataStore::flush_log, but uses the already-acquired write lock.
    pub fn flush_log(&mut self) -> Result<()> {
        self.0.log.flush()?;
        Ok(())
    }

    /// Run a function with the write guard temporarily unlocked
    ///
    /// Used when calling recursively into contentstore during add
    pub fn unlocked<U>(&mut self, f: impl FnOnce() -> U) -> U {
        RwLockWriteGuard::unlocked(&mut self.0, f)
    }
}

impl std::convert::From<crate::memcache::McData> for Entry {
    fn from(v: crate::memcache::McData) -> Self {
        Entry::new(v.key, v.data, v.metadata)
    }
}

impl std::convert::TryFrom<Entry> for crate::memcache::McData {
    type Error = anyhow::Error;

    fn try_from(mut v: Entry) -> Result<Self, Self::Error> {
        let data = v.content()?;

        Ok(crate::memcache::McData {
            key: v.key,
            data,
            metadata: v.metadata,
        })
    }
}

// TODO(meyer): Remove these infallible conversions, replace with fallible or inherent in LazyFile.
impl std::convert::From<TreeEntry> for Entry {
    fn from(v: TreeEntry) -> Self {
        Entry::new(
            v.key().clone(),
            // TODO(meyer): Why does this infallible conversion exist? Push the failure to consumer of TryFrom, at worst
            v.data_unchecked().unwrap().into(),
            Metadata::default(),
        )
    }
}

impl std::convert::From<FileEntry> for Entry {
    fn from(v: FileEntry) -> Self {
        Entry::new(
            v.key().clone(),
            v.content()
                .expect("missing content")
                .data_unchecked()
                .clone()
                .into(),
            v.metadata().expect("missing content").clone(),
        )
    }
}

impl HgIdMutableDeltaStore for IndexedLogHgIdDataStore {
    fn add(&self, delta: &Delta, metadata: &Metadata) -> Result<()> {
        ensure!(delta.base.is_none(), "Deltas aren't supported.");

        let entry = Entry::new(delta.key.clone(), delta.data.clone(), metadata.clone());
        self.put_entry(entry)
    }

    fn flush(&self) -> Result<Option<Vec<PathBuf>>> {
        self.flush_log().map(|_| None)
    }
}

impl LocalStore for IndexedLogHgIdDataStore {
    fn get_missing(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        let inner = self.inner.read();
        let missing: Vec<StoreKey> = keys
            .iter()
            .filter(|k| match k {
                StoreKey::HgId(k) => {
                    if self.missing.is_missing(&k.path) {
                        warn!("Force missing: {}", k.path);
                        return true;
                    }
                    match Entry::from_log(k, &inner.log) {
                        Ok(None) | Err(_) => true,
                        Ok(Some(_)) => false,
                    }
                }
                StoreKey::Content(_, _) => true,
            })
            .cloned()
            .collect();
        Ok(missing)
    }
}

impl HgIdDataStore for IndexedLogHgIdDataStore {
    fn get(&self, key: StoreKey) -> Result<StoreResult<Vec<u8>>> {
        let key = match key {
            StoreKey::HgId(key) => key,
            content => return Ok(StoreResult::NotFound(content)),
        };

        let mut entry = match self.get_raw_entry(&key)? {
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

        let entry = match self.get_raw_entry(&key)? {
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
        let log = &self.inner.read().log;
        log.iter()
            .map(|entry| {
                let bytes = log.slice_to_bytes(entry?);
                Entry::from_bytes(bytes)
            })
            .map(|entry| Ok(entry?.key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs::remove_file;
    use std::sync::Arc;

    use minibytes::Bytes;
    use tempfile::TempDir;

    use types::testutil::*;

    use crate::{
        scmstore::{FileAttributes, FileStore},
        testutil::*,
    };

    #[test]
    fn test_empty() {
        let tempdir = TempDir::new().unwrap();
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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
            StoreType::Shared,
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

    #[test]
    fn test_scmstore_read() -> Result<()> {
        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");
        let d = delta("1234", None, k.clone());
        let meta = Default::default();

        // Setup local indexedlog
        let tmp = TempDir::new()?;
        let local = Arc::new(IndexedLogHgIdDataStore::new(
            &tmp,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?);

        local.add(&d, &meta).unwrap();
        local.flush().unwrap();

        // Set up local-only FileStore
        let mut store = FileStore::empty();
        store.indexedlog_local = Some(local.clone());

        // Attempt fetch.
        let mut fetched = store
            .fetch(std::iter::once(k.clone()), FileAttributes::CONTENT)
            .single()?
            .expect("key not found");
        assert_eq!(fetched.file_content()?.to_vec(), d.data.as_ref().to_vec());

        Ok(())
    }

    #[test]
    fn test_scmstore_write_read() -> Result<()> {
        let k = key("a", "def6f29d7b61f9cb70b2f14f79cd5c43c38e21b2");
        let d = delta("1234", None, k.clone());
        let meta = Default::default();

        // Setup local indexedlog
        let tmp = TempDir::new()?;
        let local = Arc::new(IndexedLogHgIdDataStore::new(
            &tmp,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?);

        // Set up local-only FileStore
        let mut store = FileStore::empty();
        store.indexedlog_local = Some(local.clone());

        // Write a file
        store.write_batch(std::iter::once((k.clone(), d.data.clone(), meta)))?;

        // Attempt fetch.
        let mut fetched = store
            .fetch(std::iter::once(k.clone()), FileAttributes::CONTENT)
            .single()?
            .expect("key not found");
        assert_eq!(fetched.file_content()?.to_vec(), d.data.as_ref().to_vec());

        Ok(())
    }

    #[test]
    fn test_scmstore_extstore_use() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Use,
            &ConfigSet::new(),
            StoreType::Shared,
        )?;

        let lfs_key = key("a", "1");
        let nonlfs_key = key("b", "2");
        let content = Bytes::from(&[1, 2, 3, 4][..]);
        let lfs_metadata = Metadata {
            size: None,
            flags: Some(Metadata::LFS_FLAG),
        };
        let nonlfs_metadata = Metadata {
            size: None,
            flags: None,
        };

        let lfs_entry = Entry::new(lfs_key.clone(), content.clone(), lfs_metadata);
        let nonlfs_entry = Entry::new(nonlfs_key.clone(), content.clone(), nonlfs_metadata);

        log.put_entry(lfs_entry)?;
        log.put_entry(nonlfs_entry)?;

        // Set up local-only FileStore
        let mut store = FileStore::empty();
        store.indexedlog_local = Some(Arc::new(log));
        store.extstored_policy = ExtStoredPolicy::Use;

        let mut fetched = store.fetch(
            vec![lfs_key.clone(), nonlfs_key.clone()].into_iter(),
            FileAttributes::CONTENT,
        );

        assert_eq!(
            fetched
                .complete
                .get_mut(&nonlfs_key)
                .expect("key not found")
                .file_content()?,
            content
        );

        // Note: We don't fully respect ExtStoredPolicy in scmstore. We try to resolve the pointer,
        // and if we can't we no longer return the serialized pointer. Thus, this fails with
        // "unknown metadata" trying to deserialize a malformed LFS pointer.
        assert!(format!("{:#?}", fetched.incomplete[&lfs_key][0]).contains("unknown metadata"));
        Ok(())
    }

    #[test]
    fn test_scmstore_extstore_ignore() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdDataStore::new(
            &tempdir,
            ExtStoredPolicy::Ignore,
            &ConfigSet::new(),
            StoreType::Shared,
        )?;

        let lfs_key = key("a", "1");
        let nonlfs_key = key("b", "2");
        let content = Bytes::from(&[1, 2, 3, 4][..]);
        let lfs_metadata = Metadata {
            size: None,
            flags: Some(Metadata::LFS_FLAG),
        };
        let nonlfs_metadata = Metadata {
            size: None,
            flags: None,
        };

        let lfs_entry = Entry::new(lfs_key.clone(), content.clone(), lfs_metadata);
        let nonlfs_entry = Entry::new(nonlfs_key.clone(), content.clone(), nonlfs_metadata);

        log.put_entry(lfs_entry)?;
        log.put_entry(nonlfs_entry)?;

        // Set up local-only FileStore
        let mut store = FileStore::empty();
        store.indexedlog_local = Some(Arc::new(log));
        store.extstored_policy = ExtStoredPolicy::Ignore;

        let mut fetched = store.fetch(
            vec![lfs_key.clone(), nonlfs_key.clone()].into_iter(),
            FileAttributes::CONTENT,
        );

        assert_eq!(
            fetched
                .complete
                .get_mut(&nonlfs_key)
                .expect("key not found")
                .file_content()?,
            content
        );

        assert_eq!(fetched.incomplete[&lfs_key].len(), 0);
        Ok(())
    }
}
