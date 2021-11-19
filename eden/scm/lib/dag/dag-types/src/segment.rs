/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use serde::Deserialize;
use serde::Serialize;

use crate::id::Id;

/// Base segment.
///
/// Intermediate structure between processing a Dag and constructing high level segments.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Serialize, Deserialize, Ord, PartialOrd)]
pub struct FlatSegment {
    pub low: Id,
    pub high: Id,
    pub parents: Vec<Id>,
}

use std::collections::BTreeSet;

#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Arbitrary;
#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Gen;

#[cfg(any(test, feature = "for-tests"))]
impl Arbitrary for FlatSegment {
    fn arbitrary(g: &mut Gen) -> Self {
        Self {
            low: Id::arbitrary(g),
            high: Id::arbitrary(g),
            parents: Vec::arbitrary(g),
        }
    }
}

/// These segments can be used directly in the build process of the IdDag.
/// They produced by `IdMap::assign_head` and `IdDag::all_flat_segments`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
pub struct PreparedFlatSegments {
    /// New flat segments.
    pub segments: BTreeSet<FlatSegment>,
}

impl PreparedFlatSegments {
    pub fn vertex_count(&self) -> u64 {
        let mut count = 0;
        for segment in &self.segments {
            count += segment.high.0 - segment.low.0 + 1;
        }
        count
    }

    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Return set of all (unique) parents + head + roots of flat segments.
    ///
    /// Used by the pull fast path to provide necessary "anchor" vertexes
    /// ("universally known", and ones needed by the client to make decisions)
    /// in the IdMap.
    ///
    /// Might return some extra `Id`s that are not part of parents, heads, or
    /// roots. They are useful for the client to verify the graph is the same
    /// as the server.
    ///
    /// The size of the returned `Id`s is about `O(segments)`.
    pub fn parents_head_and_roots(&self) -> BTreeSet<Id> {
        self.segments
            .iter()
            .map(|seg| {
                // `seg.high` is either a head, or a parent referred by another seg
                // `seg.low` is either a room, or something unnecessary for lazy protocol,
                // but speeds up graph shape verification (see `check_isomorphic_graph`).
                // `parents` are either "universally known", essential for lazy protocol,
                // or something necessary for the pull protocol to re-map the IdMap.
                [seg.high, seg.low]
                    .into_iter()
                    .chain(seg.parents.clone().into_iter())
            })
            .flatten()
            .collect()
    }
}
