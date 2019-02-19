// Copyright Facebook, Inc. 2018
//! Classes for constructing and serializing a datapack file and index.
//!
//! A datapack is a pair of files that contain the revision contents for various
//! file revisions in Mercurial. It contains only revision contents (like file
//! contents), not any history information.
//!
//! It consists of two files, with the following format. All bytes are in
//! network byte order (big endian).
//!
//! ```text
//!
//! .datapack
//!     The pack itself is a series of revision deltas with some basic header
//!     information on each. A revision delta may be a fulltext, represented by
//!     a deltabasenode equal to the nullid.
//!
//!     datapack = <version: 1 byte>
//!                [<revision>,...]
//!     revision = <filename len: 2 byte unsigned int>
//!                <filename>
//!                <node: 20 byte>
//!                <deltabasenode: 20 byte>
//!                <delta len: 8 byte unsigned int>
//!                <delta>
//!                <metadata-list len: 4 byte unsigned int> [1]
//!                <metadata-list>                          [1]
//!     metadata-list = [<metadata-item>, ...]
//!     metadata-item = <metadata-key: 1 byte>
//!                     <metadata-value len: 2 byte unsigned>
//!                     <metadata-value>
//!
//!     metadata-key could be METAKEYFLAG or METAKEYSIZE or other single byte
//!     value in the future.
//!
//! .dataidx
//!     The index file consists of two parts, the fanout and the index.
//!
//!     The index is a list of index entries, sorted by node (one per revision
//!     in the pack). Each entry has:
//!
//!     - node (The 20 byte node of the entry; i.e. the commit hash, file node
//!             hash, etc)
//!     - deltabase index offset (The location in the index of the deltabase for
//!                               this entry. The deltabase is the next delta in
//!                               the chain, with the chain eventually
//!                               terminating in a full-text, represented by a
//!                               deltabase offset of -1. This lets us compute
//!                               delta chains from the index, then do
//!                               sequential reads from the pack if the revision
//!                               are nearby on disk.)
//!     - pack entry offset (The location of this entry in the datapack)
//!     - pack content size (The on-disk length of this entry's pack data)
//!
//!     The fanout is a quick lookup table to reduce the number of steps for
//!     bisecting the index. It is a series of 4 byte pointers to positions
//!     within the index. It has 2^16 entries, which corresponds to hash
//!     prefixes [0000, 0001,..., FFFE, FFFF]. Example: the pointer in slot
//!     4F0A points to the index position of the first revision whose node
//!     starts with 4F0A. This saves log(2^16)=16 bisect steps.
//!
//!     dataidx = <version: 1 byte>
//!               <config: 1 byte>
//!               <fanouttable>
//!               <index>
//!     fanouttable = [<index offset: 4 byte unsigned int>,...] (2^8 or 2^16 entries)
//!     index = [<index entry>,...]
//!     indexentry = <node: 20 byte>
//!                  <deltabase location: 4 byte signed int>
//!                  <pack entry offset: 8 byte unsigned int>
//!                  <pack entry size: 8 byte unsigned int>
//!
//! ```
//! [1]: new in version 1.

use std::{
    cell::RefCell,
    fmt,
    fs::File,
    io::{Cursor, Read},
    mem::drop,
    path::{Path, PathBuf},
    sync::Arc,
};

use byteorder::{BigEndian, ReadBytesExt};
use bytes::Bytes;
use failure::{format_err, Fail, Fallible};
use memmap::{Mmap, MmapOptions};

use lz4_pyframe::decompress;
use types::{Key, Node};

use crate::dataindex::{DataIndex, DeltaBaseOffset};
use crate::datastore::{DataStore, Delta, Metadata};
use crate::repack::{IterableStore, RepackOutputType, Repackable};
use crate::sliceext::SliceExt;
use crate::store::Store;
use crate::vfs::remove_file;

#[derive(Debug, Fail)]
#[fail(display = "Datapack Error: {:?}", _0)]
struct DataPackError(String);

#[derive(Clone, PartialEq)]
pub enum DataPackVersion {
    Zero,
    One,
}

pub struct DataPack {
    mmap: Mmap,
    version: DataPackVersion,
    index: DataIndex,
    base_path: Arc<PathBuf>,
    pack_path: PathBuf,
    index_path: PathBuf,
}

pub struct DataEntry<'a> {
    offset: u64,
    filename: &'a [u8],
    node: Node,
    delta_base: Option<Node>,
    compressed_data: &'a [u8],
    data: RefCell<Option<Bytes>>,
    metadata: Metadata,
    next_offset: u64,
}

impl DataPackVersion {
    fn new(value: u8) -> Fallible<Self> {
        match value {
            0 => Ok(DataPackVersion::Zero),
            1 => Ok(DataPackVersion::One),
            _ => {
                Err(DataPackError(format!("invalid datapack version number '{:?}'", value)).into())
            }
        }
    }
}

impl From<DataPackVersion> for u8 {
    fn from(version: DataPackVersion) -> u8 {
        match version {
            DataPackVersion::Zero => 0,
            DataPackVersion::One => 1,
        }
    }
}

impl<'a> DataEntry<'a> {
    pub fn new(buf: &'a [u8], offset: u64, version: DataPackVersion) -> Fallible<Self> {
        let mut cur = Cursor::new(buf);
        cur.set_position(offset);

        // Filename
        let filename_len = cur.read_u16::<BigEndian>()? as u64;
        let filename =
            buf.get_err(cur.position() as usize..(cur.position() + filename_len) as usize)?;
        let cur_pos = cur.position();
        cur.set_position(cur_pos + filename_len);

        // Node
        let mut node_buf: [u8; 20] = Default::default();
        cur.read_exact(&mut node_buf)?;
        let node = Node::from(&node_buf);

        // Delta
        cur.read_exact(&mut node_buf)?;
        let delta_base = Node::from(&node_buf);
        let delta_base = if delta_base.is_null() {
            None
        } else {
            Some(delta_base)
        };

        let delta_len = cur.read_u64::<BigEndian>()?;
        let compressed_data =
            buf.get_err(cur.position() as usize..(cur.position() + delta_len) as usize)?;
        let data = RefCell::new(None);
        let cur_pos = cur.position();
        cur.set_position(cur_pos + delta_len);

        // Metadata
        let mut metadata = Metadata {
            flags: None,
            size: None,
        };
        if version == DataPackVersion::One {
            metadata = Metadata::read(&mut cur)?;
        }

        let next_offset = cur.position();

        Ok(DataEntry {
            offset,
            filename,
            node,
            delta_base,
            compressed_data,
            data,
            metadata,
            next_offset,
        })
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn filename(&self) -> &[u8] {
        self.filename
    }

    pub fn node(&self) -> &Node {
        &self.node
    }

    pub fn delta_base(&self) -> &Option<Node> {
        &self.delta_base
    }

    pub fn delta(&self) -> Fallible<Bytes> {
        let mut cell = self.data.borrow_mut();
        if cell.is_none() {
            *cell = Some(decompress(&self.compressed_data)?.into());
        }

        Ok(cell.as_ref().unwrap().clone())
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

impl<'a> fmt::Debug for DataEntry<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let delta = self
            .delta()
            .unwrap_or_else(|e| Bytes::from(format!("{:?}", e).as_bytes()));
        write!(
            f,
            "DataEntry {{\n  offset: {:?}\n  filename: {:?}\n  \
             node: {:?}\n  delta_base: {:?}\n  compressed_len: {:?}\n  \
             data_len: {:?}\n  data: {:?}\n  metadata: N/A\n}}",
            self.offset,
            self.filename,
            self.node,
            self.delta_base,
            self.compressed_data.len(),
            delta.len(),
            delta.iter().map(|b| *b as char).collect::<String>(),
        )
    }
}

impl DataPack {
    pub fn new(path: &Path) -> Fallible<Self> {
        let base_path = PathBuf::from(path);
        let pack_path = path.with_extension("datapack");
        let file = File::open(&pack_path)?;
        let len = file.metadata()?.len();
        if len < 1 {
            return Err(format_err!(
                "empty datapack '{:?}' is invalid",
                path.to_str().unwrap_or("<unknown>")
            ));
        }

        let mmap = unsafe { MmapOptions::new().len(len as usize).map(&file)? };
        let version = DataPackVersion::new(mmap[0])?;
        let index_path = path.with_extension("dataidx");
        Ok(DataPack {
            mmap,
            version,
            index: DataIndex::new(&index_path)?,
            base_path: Arc::new(base_path),
            pack_path,
            index_path,
        })
    }

    pub fn len(&self) -> usize {
        self.mmap.len()
    }

    pub fn read_entry(&self, offset: u64) -> Fallible<DataEntry> {
        DataEntry::new(self.mmap.as_ref(), offset, self.version.clone())
    }

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    pub fn pack_path(&self) -> &Path {
        &self.pack_path
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }
}

impl DataStore for DataPack {
    fn get(&self, _key: &Key) -> Fallible<Vec<u8>> {
        Err(format_err!(
            "DataPack doesn't support raw get(), only getdeltachain"
        ))
    }

    fn get_delta(&self, key: &Key) -> Fallible<Delta> {
        let entry = self.index.get_entry(key.node())?;
        let data_entry = self.read_entry(entry.pack_entry_offset())?;

        Ok(Delta {
            data: data_entry.delta()?,
            base: data_entry
                .delta_base()
                .map(|delta_base| Key::new(key.name().into(), delta_base.clone())),
            key: Key::new(key.name().into(), data_entry.node().clone()),
        })
    }

    fn get_delta_chain(&self, key: &Key) -> Fallible<Vec<Delta>> {
        let mut chain: Vec<Delta> = Default::default();
        let mut next_entry = self.index.get_entry(key.node())?;
        loop {
            let data_entry = self.read_entry(next_entry.pack_entry_offset())?;
            chain.push(Delta {
                data: data_entry.delta()?,
                base: data_entry
                    .delta_base()
                    .map(|delta_base| Key::new(key.name().into(), delta_base.clone())),
                key: Key::new(key.name().into(), data_entry.node().clone()),
            });

            if let DeltaBaseOffset::Offset(offset) = next_entry.delta_base_offset() {
                next_entry = self.index.read_entry(offset as usize)?;
            } else {
                break;
            }
        }

        Ok(chain)
    }

    fn get_meta(&self, key: &Key) -> Fallible<Metadata> {
        let index_entry = self.index.get_entry(key.node())?;
        Ok(self.read_entry(index_entry.pack_entry_offset())?.metadata)
    }
}

impl Store for DataPack {
    fn from_path(path: &Path) -> Fallible<Self> {
        DataPack::new(path)
    }

    fn get_missing(&self, keys: &[Key]) -> Fallible<Vec<Key>> {
        Ok(keys
            .iter()
            .filter(|k| self.index.get_entry(k.node()).is_err())
            .map(|k| k.clone())
            .collect())
    }
}

impl IterableStore for DataPack {
    fn iter<'a>(&'a self) -> Box<Iterator<Item = Fallible<Key>> + 'a> {
        Box::new(DataPackIterator::new(self))
    }
}

impl Repackable for DataPack {
    fn delete(self) -> Fallible<()> {
        // On some platforms, removing a file can fail if it's still opened or mapped, let's make
        // sure we close and unmap them before deletion.
        drop(self.mmap);
        drop(self.index);

        let result1 = remove_file(&self.pack_path);
        let result2 = remove_file(&self.index_path);
        // Only check for errors after both have run. That way if pack_path doesn't exist,
        // index_path is still deleted.
        result1?;
        result2?;
        Ok(())
    }

    fn id(&self) -> &Arc<PathBuf> {
        &self.base_path
    }

    fn kind(&self) -> RepackOutputType {
        RepackOutputType::Data
    }
}

struct DataPackIterator<'a> {
    pack: &'a DataPack,
    offset: u64,
}

impl<'a> DataPackIterator<'a> {
    pub fn new(pack: &'a DataPack) -> Self {
        DataPackIterator {
            pack,
            offset: 1, // Start after the header byte
        }
    }
}

impl<'a> Iterator for DataPackIterator<'a> {
    type Item = Fallible<Key>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset as usize >= self.pack.len() {
            return None;
        }
        let entry = self.pack.read_entry(self.offset);
        Some(match entry {
            Ok(ref e) => {
                self.offset = e.next_offset;
                Ok(Key::new(e.filename.to_vec(), e.node))
            }
            Err(e) => Err(e),
        })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    use std::rc::Rc;

    use quickcheck::quickcheck;
    use rand::SeedableRng;
    use rand_chacha::ChaChaRng;
    use tempfile::TempDir;

    use types::node::Node;

    use crate::datastore::{Delta, Metadata};
    use crate::mutabledatapack::MutableDataPack;
    use crate::mutablepack::MutablePack;

    pub fn make_datapack(tempdir: &TempDir, deltas: &Vec<(Delta, Option<Metadata>)>) -> DataPack {
        let mut mutdatapack = MutableDataPack::new(tempdir.path(), DataPackVersion::One).unwrap();
        for &(ref delta, ref metadata) in deltas.iter() {
            mutdatapack.add(&delta, metadata.clone()).unwrap();
        }

        let path = mutdatapack.close().unwrap();

        DataPack::new(&path).unwrap()
    }

    #[test]
    fn test_empty() {
        let tempdir = TempDir::new().unwrap();
        let pack = make_datapack(&tempdir, &vec![]);
        assert!(pack.len() > 0);
    }

    #[test]
    fn test_get_missing() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![(
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: Some(Key::new(vec![0], Node::random(&mut rng))),
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        )];
        let pack = make_datapack(&tempdir, &revisions);
        for &(ref delta, ref _metadata) in revisions.iter() {
            let missing = pack.get_missing(&[delta.key.clone()]).unwrap();
            assert_eq!(missing.len(), 0);
        }

        let not = Key::new(vec![1], Node::random(&mut rng));
        let missing = pack.get_missing(&vec![not.clone()]).unwrap();
        assert_eq!(missing, vec![not.clone()]);
    }

    #[test]
    fn test_get_meta() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                Some(Metadata {
                    size: Some(1000),
                    flags: Some(7),
                }),
            ),
        ];

        let pack = make_datapack(&tempdir, &revisions);
        for &(ref delta, ref metadata) in revisions.iter() {
            let meta = pack.get_meta(&delta.key).unwrap();
            let metadata = match metadata {
                &Some(ref m) => m.clone(),
                &None => Default::default(),
            };
            assert_eq!(meta, metadata);
        }
    }

    #[test]
    fn test_get_delta_chain_single() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
        ];

        let pack = make_datapack(&tempdir, &revisions);
        for &(ref delta, ref _metadata) in revisions.iter() {
            let chain = pack.get_delta_chain(&delta.key).unwrap();
            assert_eq!(chain[0], *delta);
        }
    }

    #[test]
    fn test_get_delta() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
        ];

        let pack = make_datapack(&tempdir, &revisions);
        for &(ref expected_delta, _) in revisions.iter() {
            let delta = pack.get_delta(&expected_delta.key).unwrap();
            assert_eq!(expected_delta, &delta);
        }
    }

    #[test]
    fn test_get_delta_chain_multiple() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let mut revisions = vec![(
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: Some(Key::new(vec![0], Node::random(&mut rng))),
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        )];
        let base0 = revisions[0].0.key.clone();
        revisions.push((
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: Some(base0),
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        ));
        let base1 = revisions[1].0.key.clone();
        revisions.push((
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: Some(base1),
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        ));

        let pack = make_datapack(&tempdir, &revisions);

        let chains = [
            vec![revisions[0].0.clone()],
            vec![revisions[1].0.clone(), revisions[0].0.clone()],
            vec![
                revisions[2].0.clone(),
                revisions[1].0.clone(),
                revisions[0].0.clone(),
            ],
        ];

        for i in 0..2 {
            let chain = pack.get_delta_chain(&revisions[i].0.key).unwrap();
            assert_eq!(&chains[i], &chain);
        }
    }

    #[test]
    fn test_iter() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
            (
                Delta {
                    data: Bytes::from(&[1, 2, 3, 4][..]),
                    base: Some(Key::new(vec![0], Node::random(&mut rng))),
                    key: Key::new(vec![0], Node::random(&mut rng)),
                },
                None,
            ),
        ];

        let pack = make_datapack(&tempdir, &revisions);
        assert_eq!(
            pack.iter().collect::<Fallible<Vec<Key>>>().unwrap(),
            revisions
                .iter()
                .map(|d| d.0.key.clone())
                .collect::<Vec<Key>>()
        );
    }

    #[test]
    fn test_delete() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![(
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: None,
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        )];

        let pack = make_datapack(&tempdir, &revisions);
        assert_eq!(
            tempdir.path().read_dir().unwrap().collect::<Vec<_>>().len(),
            2
        );
        pack.delete().unwrap();
        assert_eq!(
            tempdir.path().read_dir().unwrap().collect::<Vec<_>>().len(),
            0
        );
    }

    #[test]
    fn test_delete_while_open() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![(
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: None,
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        )];

        let pack = make_datapack(&tempdir, &revisions);
        let pack2 = DataPack::new(pack.base_path()).unwrap();
        assert!(pack.delete().is_ok());
        assert!(!pack2.pack_path().exists());
        assert!(!pack2.index_path().exists());
    }

    #[test]
    fn test_rc() {
        let mut rng = ChaChaRng::from_seed([0u8; 32]);
        let tempdir = TempDir::new().unwrap();

        let revisions = vec![(
            Delta {
                data: Bytes::from(&[1, 2, 3, 4][..]),
                base: None,
                key: Key::new(vec![0], Node::random(&mut rng)),
            },
            None,
        )];

        let pack = Rc::new(make_datapack(&tempdir, &revisions));
        let delta = pack.get_delta(&revisions[0].0.key).unwrap();
        assert_eq!(delta.data, revisions[0].0.data);
    }

    quickcheck! {
        fn test_iter_quickcheck(keys: Vec<(Vec<u8>, Key)>) -> bool {
            let tempdir = TempDir::new().unwrap();

            let mut revisions: Vec<(Delta, Option<Metadata>)> = vec![];
            for (data, key) in keys {
                revisions.push((
                    Delta {
                        data: data.into(),
                        base: None,
                        key: key.clone(),
                    },
                    None,
                ));
            }

            let pack = make_datapack(&tempdir, &revisions);
            let same = pack.iter().collect::<Fallible<Vec<Key>>>().unwrap()
                == revisions
                    .iter()
                    .map(|d| d.0.key.clone())
                    .collect::<Vec<Key>>();
            same
        }
    }
}
