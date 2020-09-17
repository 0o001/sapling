/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::RwLock,
};

use anyhow::Result;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use sha1::{Digest, Sha1};

use configparser::{
    config::ConfigSet,
    hg::{ByteCount, ConfigSetHgExt},
};
use indexedlog::{
    log::IndexOutput,
    rotate::{OpenOptions, RotateLog},
    DefaultOpenOptions,
};
use types::{
    hgid::{ReadHgIdExt, WriteHgIdExt},
    HgId, Key, NodeInfo, RepoPath, RepoPathBuf,
};

use crate::{
    historystore::{HgIdHistoryStore, HgIdMutableHistoryStore},
    localstore::LocalStore,
    repack::ToKeys,
    sliceext::SliceExt,
    types::StoreKey,
};

struct IndexedLogHgIdHistoryStoreInner {
    log: RotateLog,
}

pub struct IndexedLogHgIdHistoryStore {
    inner: RwLock<IndexedLogHgIdHistoryStoreInner>,
}

struct Entry {
    key: Key,

    p1: HgId,
    p2: HgId,
    linknode: HgId,
    copy_from: Option<RepoPathBuf>,
}

impl Entry {
    pub fn new(key: &Key, info: &NodeInfo) -> Self {
        // Loops in the graph aren't allowed. Since this is a logic error in the code, let's
        // assert.
        assert_ne!(key.hgid, info.parents[0].hgid);
        assert_ne!(key.hgid, info.parents[1].hgid);

        let copy_from = if info.parents[0].path != key.path {
            Some(info.parents[0].path.to_owned())
        } else {
            None
        };

        Entry {
            key: key.clone(),
            p1: info.parents[0].hgid,
            p2: info.parents[1].hgid,
            linknode: info.linknode,
            copy_from,
        }
    }

    fn key_to_index_key(key: &Key) -> Vec<u8> {
        let mut hasher = Sha1::new();
        let path_buf: &[u8] = key.path.as_ref();
        hasher.input(path_buf);
        let buf: [u8; 20] = hasher.result().into();

        let mut index_key = Vec::with_capacity(HgId::len() * 2);
        index_key.extend_from_slice(key.hgid.as_ref());
        index_key.extend_from_slice(&buf);

        index_key
    }

    /// Read an entry from the slice and deserialize it.
    ///
    /// The on-disk format of an entry is the following:
    /// - HgId: <20 bytes>
    /// - Sha1(path) <20 bytes>
    /// - Path len: 2 unsigned bytes, big-endian
    /// - Path: <Path len> bytes
    /// - p1 hgid: <20 bytes>
    /// - p2 hgid: <20 bytes>
    /// - linknode: <20 bytes>
    /// Optionally:
    /// - copy from len: 2 unsigned bytes, big-endian
    /// - copy from: <copy from len> bytes
    fn from_slice(data: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(data);
        let hgid = cur.read_hgid()?;

        // Jump over the hashed path.
        cur.set_position(40);

        let path_len = cur.read_u16::<BigEndian>()? as u64;
        let path_slice =
            data.get_err(cur.position() as usize..(cur.position() + path_len) as usize)?;
        cur.set_position(cur.position() + path_len);
        let path = RepoPath::from_utf8(path_slice)?;

        let key = Key::new(path.to_owned(), hgid);

        let p1 = cur.read_hgid()?;
        let p2 = cur.read_hgid()?;
        let linknode = cur.read_hgid()?;

        let copy_from = if let Ok(copy_from_len) = cur.read_u16::<BigEndian>() {
            let copy_from_slice = data.get_err(
                cur.position() as usize..(cur.position() + copy_from_len as u64) as usize,
            )?;
            Some(RepoPath::from_utf8(copy_from_slice)?.to_owned())
        } else {
            None
        };

        Ok(Entry {
            key,
            p1,
            p2,
            linknode,
            copy_from,
        })
    }

    /// Read an entry from the `IndexedLog` and deserialize it.
    pub fn from_log(key: &Key, log: &RotateLog) -> Result<Option<Self>> {
        let index_key = Self::key_to_index_key(key);
        let mut log_entry = log.lookup(0, index_key)?;
        let buf = match log_entry.next() {
            None => return Ok(None),
            Some(buf) => buf?,
        };

        Self::from_slice(buf).map(Some)
    }

    /// Write an entry to the `IndexedLog`. See [`from_slice`] for the detail about the on-disk
    /// format.
    pub fn write_to_log(self, log: &mut RotateLog) -> Result<()> {
        let mut buf = Vec::new();
        buf.write_all(Self::key_to_index_key(&self.key).as_ref())?;
        let path_slice = self.key.path.as_byte_slice();
        buf.write_u16::<BigEndian>(path_slice.len() as u16)?;
        buf.write_all(path_slice)?;
        buf.write_hgid(&self.p1)?;
        buf.write_hgid(&self.p2)?;
        buf.write_hgid(&self.linknode)?;

        if let Some(copy_from) = self.copy_from {
            let copy_from_slice = copy_from.as_byte_slice();
            buf.write_u16::<BigEndian>(copy_from_slice.len() as u16)?;
            buf.write_all(copy_from_slice)?;
        }

        Ok(log.append(buf)?)
    }

    pub fn node_info(&self) -> NodeInfo {
        let p1path = if let Some(copy_from) = &self.copy_from {
            copy_from.clone()
        } else {
            self.key.path.clone()
        };

        NodeInfo {
            parents: [
                Key::new(p1path, self.p1),
                Key::new(self.key.path.clone(), self.p2),
            ],
            linknode: self.linknode,
        }
    }
}

impl IndexedLogHgIdHistoryStore {
    /// Create or open an `IndexedLogHgIdHistoryStore`.
    pub fn new(path: impl AsRef<Path>, config: &ConfigSet) -> Result<Self> {
        let mut open_options = Self::default_open_options();
        if let Some(max_bytes_per_log) =
            config.get_opt::<ByteCount>("indexedlog", "history.max-bytes-per-log")?
        {
            open_options = open_options.max_bytes_per_log(max_bytes_per_log.value());
        }
        if let Some(max_log_count) = config.get_opt::<u8>("indexedlog", "history.max-log-count")? {
            open_options = open_options.max_log_count(max_log_count);
        }
        let log = open_options.open(&path)?;
        Ok(IndexedLogHgIdHistoryStore {
            inner: RwLock::new(IndexedLogHgIdHistoryStoreInner { log }),
        })
    }
}

impl DefaultOpenOptions<OpenOptions> for IndexedLogHgIdHistoryStore {
    /// Default configuration: 4 x 0.5GB.
    fn default_open_options() -> OpenOptions {
        OpenOptions::new()
            .max_log_count(4)
            .max_bytes_per_log(500 * 1000 * 1000)
            .auto_sync_threshold(Some(250 * 1024 * 1024))
            .create(true)
            .index("node_and_path", |_| {
                vec![IndexOutput::Reference(0..(HgId::len() * 2) as u64)]
            })
    }
}

impl LocalStore for IndexedLogHgIdHistoryStore {
    fn get_missing(&self, keys: &[StoreKey]) -> Result<Vec<StoreKey>> {
        let inner = self.inner.read().unwrap();
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

impl HgIdHistoryStore for IndexedLogHgIdHistoryStore {
    fn get_node_info(&self, key: &Key) -> Result<Option<NodeInfo>> {
        let inner = self.inner.read().unwrap();
        let entry = match Entry::from_log(key, &inner.log)? {
            None => return Ok(None),
            Some(entry) => entry,
        };
        Ok(Some(entry.node_info()))
    }

    fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

impl HgIdMutableHistoryStore for IndexedLogHgIdHistoryStore {
    fn add(&self, key: &Key, info: &NodeInfo) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        let entry = Entry::new(key, info);
        entry.write_to_log(&mut inner.log)
    }

    fn flush(&self) -> Result<Option<Vec<PathBuf>>> {
        self.inner.write().unwrap().log.flush()?;
        Ok(None)
    }
}

impl ToKeys for IndexedLogHgIdHistoryStore {
    fn to_keys(&self) -> Vec<Result<Key>> {
        self.inner
            .read()
            .unwrap()
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

    use rand::SeedableRng;
    use rand_chacha::ChaChaRng;
    use tempfile::TempDir;

    use types::testutil::*;

    use crate::historypack::tests::get_nodes;

    #[test]
    fn test_empty() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        log.flush()?;
        Ok(())
    }

    #[test]
    fn test_add() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        let k = key("a", "1");
        let nodeinfo = NodeInfo {
            parents: [key("a", "2"), null_key("a")],
            linknode: hgid("3"),
        };

        log.add(&k, &nodeinfo)?;
        log.flush()?;
        Ok(())
    }

    #[test]
    fn test_add_get_node_info() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        let k = key("a", "1");
        let nodeinfo = NodeInfo {
            parents: [key("a", "2"), null_key("a")],
            linknode: hgid("3"),
        };
        log.add(&k, &nodeinfo)?;
        log.flush()?;

        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        let read_nodeinfo = log.get_node_info(&k)?;
        assert_eq!(Some(nodeinfo), read_nodeinfo);
        Ok(())
    }

    #[test]
    fn test_corrupted() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        let mut rng = ChaChaRng::from_seed([0u8; 32]);

        let nodes = get_nodes(&mut rng);
        for (key, info) in nodes.iter() {
            log.add(&key, &info)?;
        }
        log.flush()?;
        drop(log);

        // Corrupt the log by removing the "log" file.
        let mut rotate_log_path = tempdir.path().to_path_buf();
        rotate_log_path.push("0");
        rotate_log_path.push("log");
        remove_file(rotate_log_path)?;

        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        for (key, info) in nodes.iter() {
            log.add(&key, &info)?;
        }
        log.flush()?;

        assert_eq!(log.to_keys().len(), nodes.iter().count());
        Ok(())
    }

    #[test]
    fn test_iter() -> Result<()> {
        let tempdir = TempDir::new()?;
        let log = IndexedLogHgIdHistoryStore::new(&tempdir, &ConfigSet::new())?;
        let k = key("a", "1");
        let nodeinfo = NodeInfo {
            parents: [key("a", "2"), null_key("a")],
            linknode: hgid("3"),
        };
        log.add(&k, &nodeinfo)?;

        assert!(log.to_keys().into_iter().all(|e| e.unwrap() == k));
        Ok(())
    }
}
