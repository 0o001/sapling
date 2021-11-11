/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fmt;

use serde::Deserialize;
use serde::Serialize;

use crate::errors::bug;
use crate::errors::programming;
use crate::id::Group;
use crate::id::Id;
use crate::segment::Segment;
use crate::segment::SegmentFlags;
use crate::spanset::Span;
use crate::IdSet;
use crate::Level;
use crate::Result;

mod in_process_store;

#[cfg(any(test, feature = "indexedlog-backend"))]
pub(crate) mod indexedlog_store;

pub(crate) use in_process_store::InProcessStore;
#[cfg(any(test, feature = "indexedlog-backend"))]
pub(crate) use indexedlog_store::IndexedLogStore;

pub trait IdDagStore: Send + Sync + 'static {
    /// Maximum level segment in the store
    fn max_level(&self) -> Result<Level>;

    /// Find segment by level and head.
    fn find_segment_by_head_and_level(&self, head: Id, level: u8) -> Result<Option<Segment>>;

    /// Find flat segment containing the given id.
    fn find_flat_segment_including_id(&self, id: Id) -> Result<Option<Segment>>;

    /// Add a new segment.
    ///
    /// For simplicity, it does not check if the new segment overlaps with
    /// an existing segment (which is a logic error). Those checks can be
    /// offline.
    fn insert(
        &mut self,
        flags: SegmentFlags,
        level: Level,
        low: Id,
        high: Id,
        parents: &[Id],
    ) -> Result<()> {
        let segment = Segment::new(flags, level, low, high, parents);
        self.insert_segment(segment)
    }

    fn insert_segment(&mut self, segment: Segment) -> Result<()>;

    /// Return all ids from given groups. This is useful to implement the
    /// `all()` operation.
    ///
    /// With discontinuous segments, this might return multiple spans for
    /// a single group.
    fn all_ids_in_groups(&self, groups: &[Group]) -> Result<IdSet>;

    /// Find all ids covered by a specific level of segments.
    ///
    /// This function assumes that segments are built in order,
    /// and higher level segments do not cover more than lower
    /// levels.
    ///
    /// That is, if range `Id(x)..Id(y)` is covered by segment
    /// level `n`. Then segment level `n+1` would cover `Id(x)..Id(p)`
    /// and not cover `Id(p)..Id(y)` (x <= p <= y). In other words,
    /// the following cases are forbidden:
    ///
    /// ```plain,ignore
    ///     level n     [-------covered-------]
    ///     level n+1   [covered] gap [covered]
    ///
    ///     level n     [---covered---]
    ///     level n+1   gap [covered]
    ///
    ///     level n     [covered] gap
    ///     level n+1   [---covered---]
    /// ```
    ///
    /// The following cases are okay:
    ///
    /// ```plain,ignore
    ///     level n     [---covered---]
    ///     level n+1   [covered] gap
    ///
    ///     level n     [---covered---]
    ///     level n+1   [---covered---]
    /// ```
    fn all_ids_in_segment_level(&self, level: Level) -> Result<IdSet> {
        let all_ids = self.all_ids_in_groups(&Group::ALL)?;
        if level == 0 {
            return Ok(all_ids);
        }

        let mut result = IdSet::empty();
        for span in all_ids.as_spans() {
            // In this span:
            //
            //      [---------span--------]
            //                 seg-]
            //
            // If we found the right side of a segment, then we can
            // assume the segments cover till the left side without
            // checking the actual segments:
            //
            //      [---------span--------]
            //      [seg][...][seg-]
            let seg = self.iter_segments_descending(span.high, level)?.next();
            if let Some(seg) = seg {
                let seg = seg?;
                let seg_span = seg.span()?;
                if span.contains(seg_span.high) {
                    // sanity check
                    if !span.contains(seg_span.low) {
                        return programming(format!(
                            "span {:?} from all_ids_in_groups should cover all segment {:?}",
                            span, seg
                        ));
                    }
                    result.push(span.low..=seg_span.high);
                }
            }
        }
        Ok(result)
    }

    /// Return the next unused id for segments of the specified level.
    ///
    /// Useful for building segments incrementally.
    fn next_free_id(&self, level: Level, group: Group) -> Result<Id>;

    /// Find segments that covers `id..` range at the given level, within a same group.
    fn next_segments(&self, id: Id, level: Level) -> Result<Vec<Segment>>;

    /// Find segments that fully covers the given range. Return segments in ascending order.
    fn segments_in_span_ascending(&self, span: Span, level: Level) -> Result<Vec<Segment>> {
        let mut iter = self.iter_segments_ascending(span.low, level)?;
        let mut result = Vec::new();
        while let Some(item) = iter.next() {
            let seg = item?;
            let seg_span = seg.span()?;
            if seg_span.low >= span.low && seg_span.high <= span.high {
                result.push(seg);
            }
            if seg_span.low > span.high {
                break;
            }
        }
        Ok(result)
    }

    /// Iterate through segments at the given level in descending order.
    fn iter_segments_descending<'a>(
        &'a self,
        max_high_id: Id,
        level: Level,
    ) -> Result<Box<dyn Iterator<Item = Result<Segment>> + 'a>>;

    /// Iterate through segments at the given level in ascending order.
    fn iter_segments_ascending<'a>(
        &'a self,
        min_high_id: Id,
        level: Level,
    ) -> Result<Box<dyn Iterator<Item = Result<Segment>> + 'a + Send + Sync>>;

    /// Iterate through `(parent_id, segment)` for master flat segments
    /// that have a parent in the given span.
    ///
    /// Warning: The returned segments might have incorrect `high`s.
    /// See `indexedlog_store.rs` for details.
    fn iter_master_flat_segments_with_parent_span<'a>(
        &'a self,
        parent_span: Span,
    ) -> Result<Box<dyn Iterator<Item = Result<(Id, SegmentWithWrongHead)>> + 'a>>;

    /// Iterate through flat segments that have the given parent.
    ///
    /// Warning: The returned segments might have incorrect `high`s.
    /// See `indexedlog_store.rs` for details.
    fn iter_flat_segments_with_parent<'a>(
        &'a self,
        parent: Id,
    ) -> Result<Box<dyn Iterator<Item = Result<SegmentWithWrongHead>> + 'a>>;

    /// Remove all non master Group identifiers from the DAG.
    fn remove_non_master(&mut self) -> Result<()>;

    /// Attempt to merge the flat `segment` with the last flat segment to reduce
    /// fragmentation.
    ///
    /// ```plain,ignore
    /// [---last segment---] [---segment---]
    ///                    ^---- the only parent of segment
    /// [---merged segment-----------------]
    /// ```
    ///
    /// Return the merged segment if it's mergeable.
    fn maybe_merged_flat_segment(&self, segment: &Segment) -> Result<Option<Segment>> {
        let level = segment.level()?;
        if level != 0 {
            // Only applies to flat segments.
            return Ok(None);
        }
        if segment.has_root()? {
            // Cannot merge if segment has roots (implies no parent for a flat segment).
            return Ok(None);
        }
        let span = segment.span()?;
        let group = span.low.group();
        if group != Group::MASTER {
            // Do not merge non-master groups for simplicity.
            return Ok(None);
        }
        let parents = segment.parents()?;
        if parents.len() != 1 || parents[0] + 1 != span.low {
            // Cannot merge - span.low dos not have parent [low-1] (non linear).
            return Ok(None);
        }
        let last_segment = match self.iter_segments_descending(span.low, 0)?.next() {
            Some(Ok(s)) => s,
            _ => return Ok(None), // Cannot merge - No last flat segment.
        };
        let last_span = last_segment.span()?;
        if last_span.high + 1 != span.low {
            // Cannot merge - Two spans are not connected.
            return Ok(None);
        }

        // Can merge!

        // Sanity check: No high-level segments should cover "last_span".
        // This is because we intentionally dropped the last (incomplete)
        // high-level segment when building.
        for lv in 1..=self.max_level()? {
            if self
                .find_segment_by_head_and_level(last_span.high, lv)?
                .is_some()
            {
                return bug(format!(
                    "lv{} segment should not cover last flat segment {:?}! ({})",
                    lv, last_span, "check build_high_level_segments"
                ));
            }
        }

        // Calculate the merged segment.
        let merged = {
            let last_parents = last_segment.parents()?;
            let flags = {
                let last_flags = last_segment.flags()?;
                let flags = segment.flags()?;
                (flags & SegmentFlags::ONLY_HEAD) | (last_flags & SegmentFlags::HAS_ROOT)
            };
            Segment::new(flags, level, last_span.low, span.high, &last_parents)
        };

        tracing::debug!(
            "merge flat segments {:?} + {:?} => {:?}",
            &last_segment,
            &segment,
            &merged
        );

        Ok(Some(merged))
    }
}

/// Wrapper for `Segment` that prevents access to `high`.
#[derive(Eq, PartialEq)]
pub struct SegmentWithWrongHead(Segment);

impl fmt::Debug for SegmentWithWrongHead {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let span = self.0.span().unwrap();
        if self.0.has_root().unwrap() {
            write!(f, "R")?;
        }
        if self.0.only_head().unwrap() {
            write!(f, "H")?;
        }
        // Mask out the "high" part since it's incorrect.
        let parents = self.parents().unwrap();
        write!(f, "{}-x{:?}", span.low, parents)?;
        Ok(())
    }
}

impl SegmentWithWrongHead {
    pub(crate) fn low(&self) -> Result<Id> {
        self.0.low()
    }
    pub(crate) fn parent_count(&self) -> Result<usize> {
        self.0.parent_count()
    }
    pub(crate) fn parents(&self) -> Result<Vec<Id>> {
        self.0.parents()
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[derive(Serialize, Deserialize)]
enum StoreId {
    Master(usize),
    NonMaster(usize),
}

#[cfg(test)]
mod tests {
    use std::ops::Deref;

    use itertools::Itertools;
    use once_cell::sync::Lazy;

    use super::*;

    fn nid(id: u64) -> Id {
        Group::NON_MASTER.min_id() + id
    }
    //  0--1--2--3--4--5--10--11--12--13--N0--N1--N2--N5--N6
    //         \-6-7-8--9-/-----------------\-N3--N4--/
    static LEVEL0_HEAD2: Lazy<Segment> =
        Lazy::new(|| Segment::new(SegmentFlags::HAS_ROOT, 0 as Level, Id(0), Id(2), &[]));
    static LEVEL0_HEAD5: Lazy<Segment> =
        Lazy::new(|| Segment::new(SegmentFlags::ONLY_HEAD, 0 as Level, Id(3), Id(5), &[Id(2)]));
    static LEVEL0_HEAD9: Lazy<Segment> =
        Lazy::new(|| Segment::new(SegmentFlags::empty(), 0 as Level, Id(6), Id(9), &[Id(2)]));
    static LEVEL0_HEAD13: Lazy<Segment> = Lazy::new(|| {
        Segment::new(
            SegmentFlags::empty(),
            0 as Level,
            Id(10),
            Id(13),
            &[Id(5), Id(9)],
        )
    });

    static MERGED_LEVEL0_HEAD5: Lazy<Segment> = Lazy::new(|| {
        Segment::new(
            SegmentFlags::HAS_ROOT | SegmentFlags::ONLY_HEAD,
            0 as Level,
            Id(0),
            Id(5),
            &[],
        )
    });

    static LEVEL0_HEADN2: Lazy<Segment> =
        Lazy::new(|| Segment::new(SegmentFlags::empty(), 0 as Level, nid(0), nid(2), &[Id(13)]));
    static LEVEL0_HEADN4: Lazy<Segment> = Lazy::new(|| {
        Segment::new(
            SegmentFlags::empty(),
            0 as Level,
            nid(3),
            nid(4),
            &[nid(0), Id(9)],
        )
    });
    static LEVEL0_HEADN6: Lazy<Segment> = Lazy::new(|| {
        Segment::new(
            SegmentFlags::empty(),
            0 as Level,
            nid(5),
            nid(6),
            &[nid(2), nid(4)],
        )
    });

    static LEVEL1_HEAD13: Lazy<Segment> =
        Lazy::new(|| Segment::new(SegmentFlags::HAS_ROOT, 1 as Level, Id(0), Id(13), &[]));
    static LEVEL1_HEADN6: Lazy<Segment> = Lazy::new(|| {
        Segment::new(
            SegmentFlags::HAS_ROOT,
            1 as Level,
            nid(0),
            nid(6),
            &[Id(13)],
        )
    });

    // Helpers
    const ROOT: SegmentFlags = SegmentFlags::HAS_ROOT;
    const EMPTY: SegmentFlags = SegmentFlags::empty();

    const M: Group = Group::MASTER;
    const N: Group = Group::NON_MASTER;

    fn seg(flags: SegmentFlags, group: Group, low: u64, high: u64, parents: &[u64]) -> Segment {
        Segment::new(
            flags,
            0,
            group.min_id() + low,
            group.min_id() + high,
            &parents.iter().copied().map(Id).collect::<Vec<_>>(),
        )
    }

    /// High-level segment.
    fn hseg(
        level: Level,
        flags: SegmentFlags,
        group: Group,
        low: u64,
        high: u64,
        parents: &[u64],
    ) -> Segment {
        Segment::new(
            flags,
            level,
            group.min_id() + low,
            group.min_id() + high,
            &parents.iter().copied().map(Id).collect::<Vec<_>>(),
        )
    }

    fn fmt<T: fmt::Debug>(value: T) -> String {
        format!("{:?}", value)
    }

    fn fmt_iter<T: fmt::Debug>(iter: impl Iterator<Item = Result<T>>) -> Vec<String> {
        iter.map(|i| fmt(i.unwrap())).collect()
    }

    fn insert_segments(store: &mut dyn IdDagStore, segments: Vec<&Segment>) {
        for segment in segments {
            store.insert_segment(segment.clone()).unwrap();
        }
    }

    fn get_segments() -> Vec<&'static Segment> {
        vec![
            &LEVEL0_HEAD2,
            &LEVEL0_HEAD5,
            &LEVEL0_HEAD9,
            &LEVEL0_HEAD13,
            &LEVEL1_HEAD13,
            &LEVEL0_HEADN2,
            &LEVEL0_HEADN4,
            &LEVEL0_HEADN6,
            &LEVEL1_HEADN6,
        ]
    }

    fn segments_to_owned(segments: &[&Segment]) -> Vec<Segment> {
        segments.into_iter().cloned().cloned().collect()
    }

    fn test_find_segment_by_head_and_level(store: &dyn IdDagStore) {
        let segment = store
            .find_segment_by_head_and_level(Id(13), 1 as Level)
            .unwrap()
            .unwrap();
        assert_eq!(&segment, LEVEL1_HEAD13.deref());

        let opt_segment = store
            .find_segment_by_head_and_level(Id(2), 0 as Level)
            .unwrap();
        assert!(opt_segment.is_none());

        let segment = store
            .find_segment_by_head_and_level(Id(5), 0 as Level)
            .unwrap()
            .unwrap();
        assert_eq!(&segment, MERGED_LEVEL0_HEAD5.deref());

        let segment = store
            .find_segment_by_head_and_level(nid(2), 0 as Level)
            .unwrap()
            .unwrap();
        assert_eq!(&segment, LEVEL0_HEADN2.deref());
    }

    fn test_find_flat_segment_including_id(store: &dyn IdDagStore) {
        let segment = store
            .find_flat_segment_including_id(Id(10))
            .unwrap()
            .unwrap();
        assert_eq!(&segment, LEVEL0_HEAD13.deref());

        let segment = store
            .find_flat_segment_including_id(Id(0))
            .unwrap()
            .unwrap();
        assert_eq!(&segment, MERGED_LEVEL0_HEAD5.deref());

        let segment = store
            .find_flat_segment_including_id(Id(2))
            .unwrap()
            .unwrap();
        assert_eq!(&segment, MERGED_LEVEL0_HEAD5.deref());

        let segment = store
            .find_flat_segment_including_id(Id(5))
            .unwrap()
            .unwrap();
        assert_eq!(&segment, MERGED_LEVEL0_HEAD5.deref());

        let segment = store
            .find_flat_segment_including_id(nid(1))
            .unwrap()
            .unwrap();
        assert_eq!(&segment, LEVEL0_HEADN2.deref());
    }

    fn test_all_ids_in_groups(store: &mut dyn IdDagStore) {
        let all_id_str = |store: &dyn IdDagStore, groups| {
            format!("{:?}", store.all_ids_in_groups(groups).unwrap())
        };

        // Insert some discontinuous segments. Then query all_ids_in_groups.
        store.insert_segment(seg(ROOT, M, 10, 20, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=20");

        store.insert_segment(seg(ROOT, M, 30, 40, &[])).unwrap();
        store.insert_segment(seg(ROOT, M, 50, 60, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=20 30..=40 50..=60");

        // Insert adjacent segments and check that spans are merged.
        store.insert_segment(seg(EMPTY, M, 41, 45, &[40])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=20 30..=45 50..=60");

        store.insert_segment(seg(EMPTY, M, 46, 49, &[45])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=20 30..=60");

        store.insert_segment(seg(EMPTY, M, 61, 70, &[60])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=20 30..=70");

        store.insert_segment(seg(ROOT, M, 21, 29, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "10..=70");

        store.insert_segment(seg(ROOT, M, 0, 5, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "0..=5 10..=70");

        store.insert_segment(seg(ROOT, M, 6, 9, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "0..=70");

        // Spans in the non-master group.
        store.insert_segment(seg(EMPTY, N, 0, 10, &[])).unwrap();
        store.insert_segment(seg(EMPTY, N, 20, 30, &[])).unwrap();
        assert_eq!(all_id_str(store, &[N]), "N0..=N10 N20..=N30");
        store.insert_segment(seg(EMPTY, N, 11, 15, &[])).unwrap();
        assert_eq!(all_id_str(store, &[N]), "N0..=N15 N20..=N30");
        store.insert_segment(seg(EMPTY, N, 17, 19, &[])).unwrap();
        assert_eq!(all_id_str(store, &[N]), "N0..=N15 N17..=N30");
        store.insert_segment(seg(EMPTY, N, 16, 16, &[])).unwrap();
        assert_eq!(all_id_str(store, &[M]), "0..=70");
        assert_eq!(all_id_str(store, &[N]), "N0..=N30");
        assert_eq!(all_id_str(store, &[M, N]), "0..=70 N0..=N30");

        store.remove_non_master().unwrap();
        assert_eq!(all_id_str(store, &[N]), "");
        assert_eq!(all_id_str(store, &[M, N]), "0..=70");
    }

    fn test_all_ids_in_segment_level(store: &mut dyn IdDagStore) {
        let level_id_str = |store: &dyn IdDagStore, level| {
            format!("{:?}", store.all_ids_in_segment_level(level).unwrap())
        };

        // Insert some discontinuous segments. Then query all_ids_in_groups.
        insert_segments(
            store,
            vec![
                &seg(ROOT, M, 0, 10, &[]),
                &seg(EMPTY, M, 11, 20, &[9]),
                &seg(EMPTY, M, 21, 30, &[15]),
                &seg(ROOT, M, 50, 60, &[]),
                &seg(EMPTY, M, 61, 70, &[51]),
                &seg(EMPTY, M, 71, 75, &[51]),
                &seg(EMPTY, M, 76, 80, &[51]),
                &seg(EMPTY, M, 81, 85, &[51]),
                &seg(ROOT, M, 100, 110, &[]),
                &seg(EMPTY, M, 111, 120, &[105]),
                &seg(EMPTY, M, 121, 130, &[115]),
                &seg(ROOT, N, 0, 10, &[]),
                &seg(EMPTY, N, 11, 20, &[9]),
                &seg(EMPTY, N, 21, 30, &[15]),
                &hseg(1, ROOT, M, 0, 10, &[]),
                &hseg(1, EMPTY, M, 11, 20, &[9]),
                &hseg(1, ROOT, M, 50, 70, &[]),
                &hseg(1, EMPTY, M, 71, 80, &[51]),
                &hseg(1, ROOT, M, 100, 120, &[]),
                &hseg(1, ROOT, N, 0, 30, &[]),
                &hseg(2, ROOT, M, 50, 80, &[]),
                &hseg(2, ROOT, M, 100, 120, &[]),
            ],
        );

        assert_eq!(level_id_str(store, 0), "0..=30 50..=85 100..=130 N0..=N30");
        assert_eq!(level_id_str(store, 1), "0..=20 50..=80 100..=120 N0..=N30");
        assert_eq!(level_id_str(store, 2), "50..=80 100..=120");
        assert_eq!(level_id_str(store, 3), "");
    }

    fn test_discontinuous_merges(store: &mut dyn IdDagStore) {
        insert_segments(
            store,
            vec![
                &seg(ROOT, M, 0, 10, &[]),
                &seg(EMPTY, M, 20, 30, &[5]),
                &seg(EMPTY, M, 11, 15, &[10]),
                &seg(EMPTY, M, 31, 35, &[30]),
            ],
        );

        let iter = store.iter_segments_descending(Id(25), 0).unwrap();
        assert_eq!(fmt_iter(iter), ["R0-15[]"]);

        // 0-10 and 11-15 are merged.
        let seg = store.find_segment_by_head_and_level(Id(10), 0).unwrap();
        assert_eq!(fmt(seg), "None");
        let seg = store.find_segment_by_head_and_level(Id(15), 0).unwrap();
        assert_eq!(fmt(seg), "Some(R0-15[])");

        // 20-30 and 31-35 are merged.
        let seg = store.find_segment_by_head_and_level(Id(30), 0).unwrap();
        assert_eq!(fmt(seg), "None");
        let seg = store.find_segment_by_head_and_level(Id(35), 0).unwrap();
        assert_eq!(fmt(seg), "Some(20-35[5])");

        // 0-10 and 11-15 are merged.
        let seg = store.find_flat_segment_including_id(Id(9)).unwrap();
        assert_eq!(fmt(seg), "Some(R0-15[])");
        let seg = store.find_flat_segment_including_id(Id(14)).unwrap();
        assert_eq!(fmt(seg), "Some(R0-15[])");
        let seg = store.find_flat_segment_including_id(Id(16)).unwrap();
        assert_eq!(fmt(seg), "None");

        // 20-30 and 31-35 are merged.
        let seg = store.find_flat_segment_including_id(Id(35)).unwrap();
        assert_eq!(fmt(seg), "Some(20-35[5])");
        let seg = store.find_flat_segment_including_id(Id(36)).unwrap();
        assert_eq!(fmt(seg), "None");

        // Parent lookup.
        let iter = store.iter_flat_segments_with_parent(Id(5)).unwrap();
        assert_eq!(fmt_iter(iter), ["20-x[5]"]);
        let iter = store.iter_flat_segments_with_parent(Id(10)).unwrap();
        assert_eq!(fmt_iter(iter), [] as [String; 0]);
        let iter = store.iter_flat_segments_with_parent(Id(30)).unwrap();
        assert_eq!(fmt_iter(iter), [] as [String; 0]);
    }

    fn test_next_free_id(store: &dyn IdDagStore) {
        assert_eq!(
            store.next_free_id(0 as Level, Group::MASTER).unwrap(),
            Id(14)
        );
        assert_eq!(
            store.next_free_id(0 as Level, Group::NON_MASTER).unwrap(),
            nid(7)
        );
        assert_eq!(
            store.next_free_id(1 as Level, Group::MASTER).unwrap(),
            Id(14)
        );
        assert_eq!(
            store.next_free_id(2 as Level, Group::MASTER).unwrap(),
            Group::MASTER.min_id()
        );
    }

    fn test_next_segments(store: &dyn IdDagStore) {
        let segments = store.next_segments(Id(4), 0 as Level).unwrap();
        let expected = segments_to_owned(&[&MERGED_LEVEL0_HEAD5, &LEVEL0_HEAD9, &LEVEL0_HEAD13]);
        assert_eq!(segments, expected);

        let segments = store.next_segments(Id(14), 0 as Level).unwrap();
        assert!(segments.is_empty());

        let segments = store.next_segments(Id(0), 1 as Level).unwrap();
        let expected = segments_to_owned(&[&LEVEL1_HEAD13]);
        assert_eq!(segments, expected);

        let segments = store.next_segments(Id(0), 2 as Level).unwrap();
        assert!(segments.is_empty());
    }

    fn test_max_level(store: &dyn IdDagStore) {
        assert_eq!(store.max_level().unwrap(), 1);
    }

    fn test_empty_store_max_level(store: &dyn IdDagStore) {
        assert_eq!(store.max_level().unwrap(), 0);
    }

    fn test_iter_segments_descending(store: &dyn IdDagStore) {
        let answer = store
            .iter_segments_descending(Id(12), 0)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let expected = segments_to_owned(&[&LEVEL0_HEAD9, &MERGED_LEVEL0_HEAD5]);
        assert_eq!(answer, expected);

        let mut answer = store.iter_segments_descending(Id(1), 0).unwrap();
        assert!(answer.next().is_none());

        let answer = store
            .iter_segments_descending(Id(13), 1)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let expected = segments_to_owned(&[&LEVEL1_HEAD13]);
        assert_eq!(answer, expected);

        let mut answer = store.iter_segments_descending(Id(5), 2).unwrap();
        assert!(answer.next().is_none());
    }

    fn test_iter_segments_ascending(store: &dyn IdDagStore) {
        let answer = store
            .iter_segments_ascending(Id(12), 0)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let expected = segments_to_owned(&[
            &LEVEL0_HEAD13,
            &LEVEL0_HEADN2,
            &LEVEL0_HEADN4,
            &LEVEL0_HEADN6,
        ]);
        assert_eq!(answer, expected);

        let answer = store
            .iter_segments_ascending(Id(14), 0)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let expected = segments_to_owned(&[&LEVEL0_HEADN2, &LEVEL0_HEADN4, &LEVEL0_HEADN6]);
        assert_eq!(answer, expected);

        let mut answer = store.iter_segments_ascending(nid(7), 0).unwrap();
        assert!(answer.next().is_none());

        let answer = store
            .iter_segments_ascending(nid(3), 1)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let expected = segments_to_owned(&[&LEVEL1_HEADN6]);
        assert_eq!(answer, expected);

        let mut answer = store.iter_segments_ascending(Id(5), 2).unwrap();
        assert!(answer.next().is_none());
    }

    fn test_store_iter_master_flat_segments_with_parent_span(store: &dyn IdDagStore) {
        let mut answer = store
            .iter_master_flat_segments_with_parent_span(Id(2).into())
            .unwrap()
            .map_ok(|(_p, s)| s)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let mut answer2 = store
            .iter_master_flat_segments_with_parent_span((Id(0)..=Id(3)).into())
            .unwrap()
            .map_ok(|(p, s)| {
                assert_eq!(p, Id(2));
                s
            })
            .collect::<Result<Vec<_>>>()
            .unwrap();
        // LEVEL0_HEAD5 is not in answer because it was merged into MERGED_LEVEL0_HEAD5
        // and MERGED_LEVEL0_HEAD5 no longer has parent 2.
        let expected = segments_to_owned(&[&LEVEL0_HEAD9])
            .into_iter()
            .map(SegmentWithWrongHead)
            .collect::<Vec<_>>();
        answer.sort_by_key(|s| s.low().unwrap());
        assert_eq!(answer, expected);
        answer2.sort_by_key(|s| s.low().unwrap());
        assert_eq!(answer2, expected);

        let mut answer = store
            .iter_master_flat_segments_with_parent_span(Id(13).into())
            .unwrap();
        assert!(answer.next().is_none());

        let mut answer = store
            .iter_master_flat_segments_with_parent_span(Id(4).into())
            .unwrap();
        assert!(answer.next().is_none());

        let mut answer = store
            .iter_master_flat_segments_with_parent_span(nid(2).into())
            .unwrap();
        assert!(answer.next().is_none());
    }

    fn test_store_iter_flat_segments_with_parent(store: &dyn IdDagStore) {
        let lookup = |id: Id| -> Vec<_> {
            let mut list = store
                .iter_flat_segments_with_parent(id)
                .unwrap()
                .collect::<Result<Vec<_>>>()
                .unwrap();
            list.sort_unstable_by_key(|seg| seg.low().unwrap());
            list.into_iter().map(|s| s.0).collect()
        };

        let answer = lookup(Id(2));
        // LEVEL0_HEAD5 is not in answer because it was merged into MERGED_LEVEL0_HEAD5
        // and MERGED_LEVEL0_HEAD5 no longer has parent 2.
        let expected = segments_to_owned(&[&LEVEL0_HEAD9]);
        assert_eq!(answer, expected);

        let answer = lookup(Id(13));
        let expected = segments_to_owned(&[&LEVEL0_HEADN2]);
        assert_eq!(answer, expected);

        let answer = lookup(Id(4));
        assert!(answer.is_empty());

        let answer = lookup(nid(2));
        let expected = segments_to_owned(&[&LEVEL0_HEADN6]);
        assert_eq!(answer, expected);

        let answer = lookup(Id(9));
        let expected = segments_to_owned(&[&LEVEL0_HEAD13, &LEVEL0_HEADN4]);
        assert_eq!(answer, expected);
    }

    fn test_remove_non_master(store: &mut dyn IdDagStore) {
        store.remove_non_master().unwrap();

        assert!(
            store
                .find_segment_by_head_and_level(nid(2), 0 as Level)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .find_flat_segment_including_id(nid(1))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.next_free_id(0 as Level, Group::NON_MASTER).unwrap(),
            nid(0)
        );
        assert!(
            store
                .iter_master_flat_segments_with_parent_span(nid(2).into())
                .unwrap()
                .next()
                .is_none()
        );
    }

    fn for_each_empty_store(f: impl Fn(&mut dyn IdDagStore)) {
        let mut store = InProcessStore::new();
        tracing::debug!("testing InProcessStore");
        f(&mut store);

        #[cfg(feature = "indexedlog-backend")]
        {
            let dir = tempfile::tempdir().unwrap();
            let mut store = IndexedLogStore::open(&dir.path()).unwrap();
            tracing::debug!("testing IndexedLogStore");
            f(&mut store);
        }
    }

    fn for_each_store(f: impl Fn(&mut dyn IdDagStore)) {
        for_each_empty_store(|store| {
            insert_segments(store, get_segments());
            f(store);
        })
    }

    #[test]
    fn test_multi_stores_insert() {
        // `for_each_store` does inserts, we care that nothings panics.
        for_each_store(|_store| ())
    }

    #[test]
    fn test_multi_stores_find_segment_by_head_and_level() {
        for_each_store(|store| test_find_segment_by_head_and_level(store));
    }

    #[test]
    fn test_multi_stores_find_flat_segment_including_id() {
        for_each_store(|store| test_find_flat_segment_including_id(store));
    }

    #[test]
    fn test_multi_stores_all_ids_in_groups() {
        for_each_empty_store(|store| {
            test_all_ids_in_groups(store);
        })
    }

    #[test]
    fn test_multi_stores_all_ids_in_segment_level() {
        for_each_empty_store(|store| {
            test_all_ids_in_segment_level(store);
        })
    }

    #[test]
    fn test_multi_stores_next_free_id() {
        for_each_store(|store| test_next_free_id(store));
    }

    #[test]
    fn test_multi_stores_next_segments() {
        for_each_store(|store| test_next_segments(store));
    }

    #[test]
    fn test_multi_stores_max_level() {
        for_each_empty_store(|store| test_empty_store_max_level(store));
    }

    #[test]
    fn test_multi_stores_iter_segments_descending() {
        for_each_store(|store| test_iter_segments_descending(store));
    }

    #[test]
    fn test_multi_stores_iter_segments_ascending() {
        for_each_store(|store| test_iter_segments_ascending(store));
    }

    #[test]
    fn test_multi_stores_iter_master_flat_segments_with_parent_span() {
        for_each_store(|store| test_store_iter_master_flat_segments_with_parent_span(store));
    }

    #[test]
    fn test_multi_stores_iter_flat_segments_with_parent() {
        for_each_store(|store| test_store_iter_flat_segments_with_parent(store));
    }

    #[test]
    fn test_multi_stores_remove_non_master() {
        for_each_store(|store| test_remove_non_master(store));
    }

    #[test]
    fn test_multi_stores_discontinuous_merges() {
        for_each_empty_store(|store| test_discontinuous_merges(store));
    }
}
