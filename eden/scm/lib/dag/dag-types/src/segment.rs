/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use serde::{Deserialize, Serialize};

use crate::id::Id;

/// Base segment.
///
/// Intermediate structure between processing a Dag and constructing high level segments.
#[derive(Debug, Clone, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
pub struct FlatSegment {
    pub low: Id,
    pub high: Id,
    pub parents: Vec<Id>,
}

#[cfg(any(test, feature = "for-tests"))]
use quickcheck::{Arbitrary, Gen};
use std::collections::BTreeSet;

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
    pub segments: Vec<FlatSegment>,
}

impl PreparedFlatSegments {
    /// The id of the head.
    pub fn head_id(&self) -> Option<Id> {
        self.segments.last().map(|s| s.high)
    }

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

    /// Merge with another (newer) `AssignHeadOutcome`.
    pub fn merge(&mut self, rhs: Self) {
        if rhs.segments.is_empty() {
            return;
        }
        if self.segments.is_empty() {
            *self = rhs;
            return;
        }

        // sanity check: should be easy to verify - next_free_id provides
        // incremental ids.
        debug_assert!(self.segments.last().unwrap().high < rhs.segments[0].low);

        // NOTE: Consider merging segments for slightly better perf.
        self.segments.extend(rhs.segments);
    }

    /// Return set of all (unique) parents + head + roots of flat segments.
    pub fn parents_head_and_roots(&self) -> BTreeSet<Id> {
        // Parents
        let mut s: BTreeSet<Id> = self
            .segments
            .iter()
            .map(|seg| &seg.parents)
            .flatten()
            .copied()
            .collect();
        // Head
        if let Some(h) = self.head_id() {
            s.insert(h);
        }
        // Roots
        let id_set: BTreeSet<(Id, Id)> = self.segments.iter().map(|s| (s.low, s.high)).collect();
        let contains = |id: Id| -> bool {
            for &(low, high) in id_set.range(..=(id, Id::MAX)).rev() {
                if id >= low && id <= high {
                    return true;
                }
                if id < low {
                    break;
                }
            }
            false
        };
        for seg in &self.segments {
            let pids: Vec<Id> = seg.parents.iter().copied().collect();
            if pids.iter().all(|&p| !contains(p)) {
                // seg.low is a root.
                s.insert(seg.low);
            }
        }
        s
    }

    /// Add graph edges: id -> parent_ids. Used by `assign_head`.
    pub fn push_edge(&mut self, id: Id, parent_ids: &[Id]) {
        let new_seg = || FlatSegment {
            low: id,
            high: id,
            parents: parent_ids.to_vec(),
        };

        // sanity check: this should be easy to verify - assign_head gets new ids
        // by `next_free_id()`, which should be incremental.
        debug_assert!(
            self.segments.last().map_or(Id(0), |s| s.high + 1) < id + 1,
            "push_edge(id={}, parent_ids={:?}) called out of order ({:?})",
            id,
            parent_ids,
            self
        );

        if parent_ids.len() != 1 || parent_ids[0] + 1 != id {
            // Start a new segment.
            self.segments.push(new_seg());
        } else {
            // Try to reuse the existing last segment.
            if let Some(seg) = self.segments.last_mut() {
                if seg.high + 1 == id {
                    seg.high = id;
                } else {
                    self.segments.push(new_seg());
                }
            } else {
                self.segments.push(new_seg());
            }
        }
    }

    #[cfg(feature = "for-tests")]
    /// Verify against a parent function. For testing only.
    pub fn verify<F, E>(&self, parent_func: F)
    where
        F: Fn(Id) -> Result<Vec<Id>, E>,
        E: std::fmt::Debug,
    {
        for seg in &self.segments {
            assert_eq!(
                parent_func(seg.low).unwrap(),
                seg.parents,
                "parents mismtach for {} ({:?})",
                seg.low,
                &self
            );
            for id in (seg.low + 1).0..=seg.high.0 {
                let id = Id(id);
                assert_eq!(
                    parent_func(id).unwrap(),
                    vec![id - 1],
                    "parents mismatch for {} ({:?})",
                    id,
                    &self
                );
            }
        }
    }
}
