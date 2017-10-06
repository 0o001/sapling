// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Quickcheck support for a few types that don't have support upstream,
//! and for a few other test types.

use std::convert::From;
use std::iter;
use std::result;
use std::vec::IntoIter;

use bytes::Bytes;
use futures::stream;
use quickcheck::{empty_shrinker, Arbitrary, Gen};

use mercurial_types::{Delta, MPath, NodeHash};

use changegroup;
use errors::*;

#[derive(Clone, Debug)]
pub struct QCBytes(Bytes);

impl From<QCBytes> for Bytes {
    fn from(qcbytes: QCBytes) -> Bytes {
        qcbytes.0
    }
}

impl Arbitrary for QCBytes {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        // Just use the Vec<u8> quickcheck underneath.
        let v: Vec<u8> = Vec::arbitrary(g);
        QCBytes(v.into())
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        Box::new(self.0.to_vec().shrink().map(|v| QCBytes(v.into())))
    }
}

#[derive(Clone, Debug)]
pub struct Cg2PartSequence {
    // Storing the ends in here bypasses a number of lifetime issues.
    changesets: Vec<changegroup::Part>,
    changesets_end: changegroup::Part,
    manifests: Vec<changegroup::Part>,
    manifests_end: changegroup::Part,
    filelogs: Vec<(Vec<changegroup::Part>, changegroup::Part)>,
    end: changegroup::Part,
}

impl Cg2PartSequence {
    /// Combine all the changesets, manifests and filelogs into a single iterator.
    pub fn as_iter<'a>(&'a self) -> Box<Iterator<Item = &'a changegroup::Part> + 'a> {
        // Trying to describe the type here is madness. Just box it.
        Box::new(
            self.changesets
                .iter()
                .chain(iter::once(&self.changesets_end))
                .chain(self.manifests.iter())
                .chain(iter::once(&self.manifests_end))
                .chain(
                    self.filelogs
                        .iter()
                        .filter(|&&(ref parts, _)| {
                            // If there are no filelog parts, it isn't valid to return a
                            // SectionEnd since that won't be referring to anything. So
                            // just skip the whole filelog.
                            !parts.is_empty()
                        })
                        .flat_map(|&(ref parts, ref end)| parts.iter().chain(iter::once(end))),
                )
                .chain(iter::once(&self.end)),
        )
    }

    /// Combine all the changesets, manifests and filelogs into a single stream.
    ///
    /// This returns a clone of everything because streams can't really return
    /// references at the moment.
    pub fn to_stream(
        &self,
    ) -> stream::IterOk<IntoIter<result::Result<changegroup::Part, Error>>, Error> {
        let part_results: Vec<_> = self.as_iter().cloned().map(|x| Ok(x)).collect();
        stream::iter_ok(part_results.into_iter())
    }
}

impl PartialEq<[changegroup::Part]> for Cg2PartSequence {
    fn eq(&self, other: &[changegroup::Part]) -> bool {
        self.as_iter().eq(other.iter())
    }
}

impl Arbitrary for Cg2PartSequence {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        use changegroup::*;

        // Generate a valid part sequence (changegroup, then manifest, then filelogs).
        let size = g.size();

        let changesets = gen_parts(Section::Changeset, g);
        let manifests = gen_parts(Section::Manifest, g);

        let nfilelogs = g.gen_range(0, size);
        let mut filelogs = Vec::with_capacity(nfilelogs);

        for _ in 0..nfilelogs {
            let path = MPath::arbitrary(g);
            let section_end = Part::SectionEnd(Section::Filelog(path.clone()));
            filelogs.push((gen_parts(Section::Filelog(path), g), section_end));
        }

        Cg2PartSequence {
            changesets: changesets,
            changesets_end: Part::SectionEnd(Section::Changeset),
            manifests: manifests,
            manifests_end: Part::SectionEnd(Section::Manifest),
            filelogs: filelogs,
            end: Part::End,
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        use changegroup::*;

        // All the parts can be shrinked independently as long as the section
        // remains the same (ensured in the impl of Arbitrary for
        // changegroup::Part).
        Box::new(
            (
                self.changesets.clone(),
                self.manifests.clone(),
                self.filelogs.clone(),
            ).shrink()
                .map(|(c, m, f)| {
                    Cg2PartSequence {
                        changesets: c,
                        changesets_end: Part::SectionEnd(Section::Changeset),
                        manifests: m,
                        manifests_end: Part::SectionEnd(Section::Manifest),
                        filelogs: f,
                        end: Part::End,
                    }
                }),
        )
    }
}

fn gen_parts<G: Gen>(section: changegroup::Section, g: &mut G) -> Vec<changegroup::Part> {
    let size = g.size();
    (0..g.gen_range(0, size))
        .map(|_| {
            changegroup::Part::CgChunk(section.clone(), changegroup::CgDeltaChunk::arbitrary(g))
        })
        .collect()
}

impl Arbitrary for changegroup::Part {
    fn arbitrary<G: Gen>(_g: &mut G) -> Self {
        unimplemented!()
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        use changegroup::Part::CgChunk;

        match self {
            &CgChunk(ref section, ref delta_chunk) => {
                // Keep the section the same, but shrink the delta chunk.
                let section = section.clone();
                Box::new(
                    delta_chunk
                        .shrink()
                        .map(move |chunk| CgChunk(section.clone(), chunk)),
                )
            }
            _ => empty_shrinker(),
        }
    }
}

impl Arbitrary for changegroup::CgDeltaChunk {
    fn arbitrary<G: Gen>(g: &mut G) -> Self {
        // TODO: should these be more structured? e.g. base = p1 some of the time
        changegroup::CgDeltaChunk {
            node: NodeHash::arbitrary(g),
            p1: NodeHash::arbitrary(g),
            p2: NodeHash::arbitrary(g),
            base: NodeHash::arbitrary(g),
            linknode: NodeHash::arbitrary(g),
            delta: Delta::arbitrary(g),
        }
    }

    fn shrink(&self) -> Box<Iterator<Item = Self>> {
        // Don't bother trying to shrink node hashes -- the meat is in the delta.
        let clone = self.clone();
        Box::new(self.delta.shrink().map(move |delta| {
            changegroup::CgDeltaChunk {
                node: clone.node.clone(),
                p1: clone.p1.clone(),
                p2: clone.p2.clone(),
                base: clone.base.clone(),
                linknode: clone.linknode.clone(),
                delta: delta,
            }
        }))
    }
}
