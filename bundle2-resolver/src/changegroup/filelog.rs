// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use bytes::Bytes;
use failure::Compat;
use futures::{Future, Stream};
use futures::future::{ok, Shared};
use futures_ext::{BoxFuture, BoxStream, FutureExt, StreamExt};
use heapsize::HeapSizeOf;
use quickcheck::{Arbitrary, Gen};

use blobrepo::{BlobRepo, ContentBlobInfo, ContentBlobMeta, HgBlobEntry};
use mercurial::{self, file, HgNodeHash, HgNodeKey, NodeHashConversion};
use mercurial_bundles::changegroup::CgDeltaChunk;
use mercurial_types::{delta, manifest, Delta, FileType, HgBlob, MPath, RepoPath};

use errors::*;
use stats::*;
use upload_blobs::{UploadableBlob, UploadableHgBlob};

#[derive(Debug, Eq, PartialEq)]
pub struct FilelogDeltaed {
    pub path: MPath,
    pub chunk: CgDeltaChunk,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Filelog {
    pub node_key: HgNodeKey,
    pub p1: Option<HgNodeHash>,
    pub p2: Option<HgNodeHash>,
    pub linknode: HgNodeHash,
    pub data: Bytes,
}

impl UploadableHgBlob for Filelog {
    // * Shared is required here because a single file node can be referred to by more than
    //   one changeset, and all of those will want to refer to the corresponding future.
    // * The Compat<Error> here is because the error type for Shared (a cloneable wrapper called
    //   SharedError) doesn't implement Fail, and only implements Error if the wrapped type
    //   implements Error.
    type Value = Shared<BoxFuture<(HgBlobEntry, RepoPath), Compat<Error>>>;

    fn upload(self, repo: &BlobRepo) -> Result<(HgNodeKey, Self::Value)> {
        let node_key = self.node_key;
        let p1 = self.p1.map(|p| p.into_mononoke());
        let p2 = self.p2.map(|p| p.into_mononoke());

        repo.upload_entry(
            HgBlob::from(self.data),
            manifest::Type::File(FileType::Regular),
            p1,
            p2,
            node_key.path.clone(),
        ).map(move |(_node, fut)| (node_key, fut.map_err(Error::compat).boxify().shared()))
    }
}

impl UploadableBlob for Filelog {
    // * Shared is required here because a single content blob can be referred to by more than
    //   one changeset, and all of those will want to refer to the corresponding future.
    // * The Compat<Error> here is because the error type for Shared (a cloneable wrapper called
    //   SharedError) doesn't implement Fail, and only implements Error if the wrapped type
    //   implements Error.
    type Value = (ContentBlobInfo, Shared<BoxFuture<(), Compat<Error>>>);

    fn upload(self, repo: &BlobRepo) -> Result<(HgNodeKey, Self::Value)> {
        let node_key = self.node_key;
        let path = match node_key.path.clone() {
            RepoPath::FilePath(p) => p,
            other => bail_msg!(
                "internal error: expected RepoPath::FilePath, found {:?}",
                other
            ),
        };
        let p1 = self.p1.map(|p| p.into_mononoke());
        let p2 = self.p2.map(|p| p.into_mononoke());

        let f = file::File::new(self.data, p1.as_ref(), p2.as_ref());
        let copy_from = f.copied_from()?
            .map(|(path, hash)| (path, hash.into_mercurial()));
        let contents = f.file_contents();
        let contents_blob = contents.into_blob();
        let cbinfo = ContentBlobInfo {
            path,
            meta: ContentBlobMeta {
                id: *contents_blob.id(),
                copy_from,
            },
        };

        let fut = repo.upload_blob(contents_blob)
            .map(move |_id| ())
            .map_err(Error::compat)
            .boxify()
            .shared();
        Ok((node_key, (cbinfo, fut)))
    }
}

pub fn convert_to_revlog_filelog<S>(repo: Arc<BlobRepo>, deltaed: S) -> BoxStream<Filelog, Error>
where
    S: Stream<Item = FilelogDeltaed, Error = Error> + Send + 'static,
{
    let mut delta_cache = DeltaCache::new(repo);
    deltaed
        .and_then(move |FilelogDeltaed { path, chunk }| {
            let CgDeltaChunk {
                node,
                base,
                delta,
                p1,
                p2,
                linknode,
            } = chunk;

            delta_cache
                .decode(node.clone(), base.into_option(), delta)
                .and_then(move |data| {
                    Ok(Filelog {
                        node_key: HgNodeKey {
                            path: RepoPath::FilePath(path),
                            hash: node,
                        },
                        p1: p1.into_option(),
                        p2: p2.into_option(),
                        linknode,
                        data,
                    })
                })
                .boxify()
        })
        .boxify()
}

struct DeltaCache {
    repo: Arc<BlobRepo>,
    bytes_cache: HashMap<HgNodeHash, Shared<BoxFuture<Bytes, Compat<Error>>>>,
}

impl DeltaCache {
    fn new(repo: Arc<BlobRepo>) -> Self {
        Self {
            repo,
            bytes_cache: HashMap::new(),
        }
    }

    fn decode(
        &mut self,
        node: HgNodeHash,
        base: Option<HgNodeHash>,
        delta: Delta,
    ) -> BoxFuture<Bytes, Error> {
        let bytes = match self.bytes_cache.get(&node).cloned() {
            Some(bytes) => bytes,
            None => {
                let dsize = delta.heap_size_of_children() as i64;
                STATS::deltacache_dsize.add_value(dsize);
                STATS::deltacache_dsize_large.add_value(dsize);

                let vec = match base {
                    None => ok(delta::apply(b"", &delta)).boxify(),
                    Some(base) => {
                        let fut = match self.bytes_cache.get(&base) {
                            Some(bytes) => bytes
                                .clone()
                                .map(move |bytes| delta::apply(&bytes, &delta))
                                .map_err(Error::from)
                                .boxify(),
                            None => self.repo
                                .get_file_content(&base.into_mononoke())
                                .map(move |bytes| delta::apply(bytes.into_bytes().as_ref(), &delta))
                                .boxify(),
                        };
                        fut.map_err(move |err| {
                            Error::from(err.context(format_err!(
                                "While looking for base {:?} to apply on delta {:?}",
                                base,
                                node
                            ))).compat()
                        }).boxify()
                    }
                };

                let bytes = vec.map(|vec| Bytes::from(vec)).boxify().shared();

                if self.bytes_cache.insert(node, bytes.clone()).is_some() {
                    panic!("Logic error: byte cache returned None for HashMap::get with node");
                }
                bytes
            }
        };

        bytes
            .inspect(|bytes| {
                let fsize = (mem::size_of::<u8>() * bytes.as_ref().len()) as i64;
                STATS::deltacache_fsize.add_value(fsize);
                STATS::deltacache_fsize_large.add_value(fsize);
            })
            .map(|bytes| (*bytes).clone())
            .from_err()
            .boxify()
    }
}

impl Arbitrary for Filelog {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        Filelog {
            node_key: HgNodeKey {
                path: RepoPath::FilePath(MPath::arbitrary(g)),
                hash: HgNodeHash::arbitrary(g),
            },
            p1: HgNodeHash::arbitrary(g).into_option(),
            p2: HgNodeHash::arbitrary(g).into_option(),
            linknode: HgNodeHash::arbitrary(g),
            data: Bytes::from(Vec::<u8>::arbitrary(g)),
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        fn append(result: &mut Vec<Filelog>, f: Filelog) {
            result.append(&mut f.shrink().collect());
            result.push(f);
        }

        let mut result = Vec::new();

        if self.node_key.hash != mercurial::NULL_HASH {
            let mut f = self.clone();
            f.node_key.hash = mercurial::NULL_HASH;
            append(&mut result, f);
        }

        if self.p1 != None {
            let mut f = self.clone();
            f.p1 = None;
            append(&mut result, f);
        }

        if self.p2 != None {
            let mut f = self.clone();
            f.p2 = None;
            append(&mut result, f);
        }

        if self.linknode != mercurial::NULL_HASH {
            let mut f = self.clone();
            f.linknode = mercurial::NULL_HASH;
            append(&mut result, f);
        }

        if self.data.len() != 0 {
            let mut f = self.clone();
            f.data = Bytes::from(Vec::new());
            append(&mut result, f);
        }

        Box::new(result.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cmp::min;

    use futures::Future;
    use futures::stream::iter_ok;
    use itertools::{assert_equal, EitherOrBoth, Itertools};

    use mercurial::NULL_HASH;
    use mercurial::mocks::*;
    use mercurial_types::delta::Fragment;

    struct NodeHashGen {
        bytes: Vec<u8>,
    }

    impl NodeHashGen {
        fn new() -> Self {
            Self {
                bytes: Vec::from(NULL_HASH.as_ref()),
            }
        }

        fn next(&mut self) -> HgNodeHash {
            for i in 0..self.bytes.len() {
                if self.bytes[i] == 255 {
                    self.bytes[i] = 0;
                } else {
                    self.bytes[i] = self.bytes[i] + 1;
                    return HgNodeHash::from_bytes(self.bytes.as_slice()).unwrap();
                }
            }

            panic!("NodeHashGen overflow");
        }
    }

    fn check_conversion<I, J>(inp: I, exp: J)
    where
        I: IntoIterator<Item = FilelogDeltaed>,
        J: IntoIterator<Item = Filelog>,
    {
        let result = convert_to_revlog_filelog(
            Arc::new(BlobRepo::new_memblob_empty(None, None).unwrap()),
            iter_ok(inp.into_iter().collect::<Vec<_>>()),
        ).collect()
            .wait()
            .unwrap();

        assert_equal(result, exp);
    }

    fn filelog_to_deltaed(f: &Filelog) -> FilelogDeltaed {
        FilelogDeltaed {
            path: f.node_key.path.mpath().unwrap().clone(),
            chunk: CgDeltaChunk {
                node: f.node_key.hash.clone(),
                p1: f.p1.clone().unwrap_or(NULL_HASH),
                p2: f.p2.clone().unwrap_or(NULL_HASH),
                base: NULL_HASH,
                linknode: f.linknode.clone(),
                delta: Delta::new_fulltext(f.data.as_ref()),
            },
        }
    }

    fn compute_delta(b1: &[u8], b2: &[u8]) -> Delta {
        let mut frags = Vec::new();
        let mut start = 0;
        let mut frag = Vec::new();
        for (idx, val) in b1.iter().zip_longest(b2.iter()).enumerate() {
            match val {
                EitherOrBoth::Both(v1, v2) => {
                    if v1 == v2 && !frag.is_empty() {
                        frags.push(Fragment {
                            start,
                            end: start + frag.len(),
                            content: mem::replace(&mut frag, Vec::new()),
                        });
                    } else if v1 != v2 {
                        if frag.is_empty() {
                            start = idx;
                        }
                        frag.push(*v2);
                    }
                }
                EitherOrBoth::Left(_) => continue,
                EitherOrBoth::Right(v) => {
                    if frag.is_empty() {
                        start = idx;
                    }
                    frag.push(*v)
                }
            }
        }
        if !frag.is_empty() {
            frags.push(Fragment {
                start,
                end: min(start + frag.len(), b1.len()),
                content: mem::replace(&mut frag, Vec::new()),
            });
        }
        if b1.len() > b2.len() {
            frags.push(Fragment {
                start: b2.len(),
                end: b1.len(),
                content: Vec::new(),
            });
        }

        Delta::new(frags).unwrap()
    }

    #[test]
    fn two_fulltext_files() {
        let f1 = Filelog {
            node_key: HgNodeKey {
                path: RepoPath::FilePath(MPath::new(b"test").unwrap()),
                hash: ONES_HASH,
            },
            p1: Some(TWOS_HASH),
            p2: Some(THREES_HASH),
            linknode: FOURS_HASH,
            data: Bytes::from("test file content"),
        };

        let f2 = Filelog {
            node_key: HgNodeKey {
                path: RepoPath::FilePath(MPath::new(b"test2").unwrap()),
                hash: FIVES_HASH,
            },
            p1: Some(SIXES_HASH),
            p2: Some(SEVENS_HASH),
            linknode: EIGHTS_HASH,
            data: Bytes::from("test2 file content"),
        };

        check_conversion(
            vec![filelog_to_deltaed(&f1), filelog_to_deltaed(&f2)],
            vec![f1, f2],
        );
    }

    fn files_check_order(correct_order: bool) {
        let f1 = Filelog {
            node_key: HgNodeKey {
                path: RepoPath::FilePath(MPath::new(b"test").unwrap()),
                hash: ONES_HASH,
            },
            p1: Some(TWOS_HASH),
            p2: Some(THREES_HASH),
            linknode: FOURS_HASH,
            data: Bytes::from("test file content"),
        };

        let f2 = Filelog {
            node_key: HgNodeKey {
                path: RepoPath::FilePath(MPath::new(b"test2").unwrap()),
                hash: FIVES_HASH,
            },
            p1: Some(SIXES_HASH),
            p2: Some(SEVENS_HASH),
            linknode: EIGHTS_HASH,
            data: Bytes::from("test2 file content"),
        };

        let f1_deltaed = filelog_to_deltaed(&f1);
        let mut f2_deltaed = filelog_to_deltaed(&f2);

        f2_deltaed.chunk.base = f1.node_key.hash.clone();
        f2_deltaed.chunk.delta = compute_delta(&f1.data, &f2.data);

        let inp = if correct_order {
            vec![f1_deltaed, f2_deltaed]
        } else {
            vec![f2_deltaed, f1_deltaed]
        };

        let result = convert_to_revlog_filelog(
            Arc::new(BlobRepo::new_memblob_empty(None, None).unwrap()),
            iter_ok(inp),
        ).collect()
            .wait();

        match result {
            Ok(_) => assert!(
                correct_order,
                "Successfuly converted even though order was incorrect"
            ),
            Err(_) => assert!(
                !correct_order,
                "Filed to convert even though order was correct"
            ),
        }
    }

    #[test]
    fn files_order_correct() {
        files_check_order(true);
    }

    #[test]
    fn files_order_incorrect() {
        files_check_order(false);
    }

    quickcheck! {
        fn sanitycheck_delta_computation(b1: Vec<u8>, b2: Vec<u8>) -> bool {
            assert_equal(&b2, &delta::apply(&b1, &compute_delta(&b1, &b2)));
            true
        }

        fn correct_conversion_single(f: Filelog) -> bool {
            check_conversion(
                vec![filelog_to_deltaed(&f)],
                vec![f],
            );

            true
        }

        fn correct_conversion_delta_against_first(f: Filelog, fs: Vec<Filelog>) -> bool {
            let mut hash_gen = NodeHashGen::new();

            let mut f = f.clone();
            f.node_key.hash = hash_gen.next();

            let mut fs = fs.clone();
            for el in fs.iter_mut() {
                el.node_key.hash = hash_gen.next();
            }

            let mut deltas = vec![filelog_to_deltaed(&f)];
            for filelog in &fs {
                let mut delta = filelog_to_deltaed(filelog);
                delta.chunk.base = f.node_key.hash.clone();
                delta.chunk.delta =
                    compute_delta(&f.data, &filelog.data);
                deltas.push(delta);
            }

            check_conversion(deltas, vec![f].into_iter().chain(fs));

            true
        }

        fn correct_conversion_delta_against_next(fs: Vec<Filelog>) -> bool {
            let mut hash_gen = NodeHashGen::new();

            let mut fs = fs.clone();
            for el in fs.iter_mut() {
                el.node_key.hash = hash_gen.next();
            }

            let deltas = {
                let mut it = fs.iter();
                let mut deltas = match it.next() {
                    None => return true, // empty test case
                    Some(f) => vec![filelog_to_deltaed(f)],
                };

                for (prev, next) in fs.iter().zip(it) {
                    let mut delta = filelog_to_deltaed(next);
                    delta.chunk.base = prev.node_key.hash.clone();
                    delta.chunk.delta =
                        compute_delta(&prev.data, &next.data);
                    deltas.push(delta);
                }

                deltas
            };

            check_conversion(deltas, fs);

            true
        }
    }
}
