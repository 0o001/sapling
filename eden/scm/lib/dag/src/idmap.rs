/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! # idmap
//!
//! See [`IdMap`] for the main structure.

use crate::id::Group;
use crate::id::Id;
use crate::id::VertexName;
use crate::ops::IdConvert;
use crate::ops::Parents;
use crate::segment::PreparedFlatSegments;
use crate::IdSet;
use crate::Result;

#[cfg(any(test, feature = "indexedlog-backend"))]
mod indexedlog_idmap;
mod mem_idmap;

#[cfg(any(test, feature = "indexedlog-backend"))]
pub use indexedlog_idmap::IdMap;
pub(crate) use mem_idmap::CoreMemIdMap;
pub use mem_idmap::MemIdMap;

/// DAG-aware write operations.
#[async_trait::async_trait]
pub trait IdMapAssignHead: IdConvert + IdMapWrite {
    /// Assign an id for a head in a DAG. This implies ancestors of the
    /// head will also have ids assigned.
    ///
    /// This function is incremental. If the head or any of its ancestors
    /// already have an id stored in this map, the existing ids will be
    /// reused.
    ///
    /// This function needs roughly `O(N)` heap memory. `N` is the number of
    /// ids to assign. When `N` is very large, try assigning ids to a known
    /// ancestor first.
    ///
    /// New `id`s inserted by this function will have the specified `group`.
    /// Existing `id`s that are ancestors of `head` will get re-assigned
    /// if they have a higher `group`.
    ///
    /// `covered_ids` specifies what ranges of `Id`s are already covered.
    /// This is usually obtained from `IdDag::all_ids_in_groups(&Group::ALL)`.
    /// `IdMap` itself might not be able to provide that information
    /// efficiently because it might be lazy. `covered_ids` will be updated
    /// to cover newly inserted `Id`s.
    ///
    /// `reserved_ids` specifies what ranges are reserved for future growth
    /// of other important heads (usually a couple of mainline branches that
    /// are long-lived, growing, and used by many people). This is useful
    /// to reduce fragmentation.
    async fn assign_head(
        &mut self,
        head: VertexName,
        parents_by_name: &dyn Parents,
        group: Group,
        covered_ids: &mut IdSet,
        reserved_ids: &IdSet,
    ) -> Result<PreparedFlatSegments> {
        assert!(reserved_ids.is_empty());
        // Use `covered_ids` to calculate next free id.
        let mut next_free_id = match covered_ids
            .intersection(&IdSet::from(group.min_id()..=group.max_id()))
            .max()
        {
            Some(id) => id + 1,
            None => group.min_id(),
        };

        // There are some interesting cases to optimize the numbers:
        //
        // C     For a merge C, it has choice to assign numbers to A or B
        // |\    first (A and B are abstract branches that have many nodes).
        // A B   Suppose branch A is linear and B have merges, and D is
        // |/    (::A & ::B). Then:
        // D
        //
        // - If `D` is empty or already assigned, it's better to assign A last.
        //   This is because (A+C) can then always form a segment regardless of
        //   the complexity of B:
        //
        //      B   A   C       vs.        A   B   C
        //     ~~~  ^^^^^                     ~~~
        //     xxxxxx                          *****
        //                                 xxxxx
        //
        //   [~]: Might be complex (ex. many segments)
        //   [^]: Can always form a segment. (better)
        //   [*]: Can only be a segment if segment size is large enough.
        //   [x]: Cannot form a segment.
        //
        // - If `D` is not empty (and not assigned), it _might_ be better to
        //   assign D and A first. This provides benefits for A and D to be
        //   continuous, with the downside that A and C are not continuous.
        //
        //   A typical pattern is one branch continuously merges into the other
        //   (see also segmented-changelog.pdf, page 19):
        //
        //        B---D---F
        //         \   \   \
        //      A---C---E---G
        //
        // The code below is optimized for cases where p1 branch is linear,
        // but p2 branch is not.
        let mut outcome = PreparedFlatSegments::default();

        // Emulate the stack in heap to avoid overflow.
        #[derive(Debug)]
        enum Todo {
            /// Visit parents. Finally assign self. This will eventually turn into AssignedId.
            Visit(VertexName),

            /// Assign a number if not assigned. Parents are visited.
            /// The `usize` provides the length of parents.
            Assign(VertexName, usize),

            /// Assigned Id. Will be picked by and pushed to the current `parent_ids` stack.
            AssignedId(Id),
        }
        use Todo::Assign;
        use Todo::AssignedId;
        use Todo::Visit;
        let mut parent_ids: Vec<Id> = Vec::new();

        let mut todo_stack: Vec<Todo> = vec![Visit(head.clone())];
        while let Some(todo) = todo_stack.pop() {
            tracing::trace!(target: "dag::assign", "todo: {:?}", &todo);
            match todo {
                Visit(head) => {
                    // If the id was not assigned, or was assigned to a higher group,
                    // (re-)assign it to this group.
                    //
                    // PERF: This might trigger remote fetch too frequently.
                    match self.vertex_id_with_max_group(&head, group).await? {
                        None => {
                            let parents = parents_by_name.parent_names(head.clone()).await?;
                            tracing::trace!(target: "dag::assign", "visit {:?} with parents {:?}", &head, &parents);
                            todo_stack.push(Todo::Assign(head, parents.len()));
                            // If the parent was not assigned, or was assigned to a higher group,
                            // (re-)assign the parent to this group.
                            // "rev" is the "optimization"
                            for p in parents.into_iter().rev() {
                                match self.vertex_id_with_max_group(&p, group).await {
                                    Ok(Some(id)) => todo_stack.push(Todo::AssignedId(id)),
                                    Ok(None) => todo_stack.push(Todo::Visit(p)),
                                    Err(e) => return Err(e),
                                }
                            }
                        }
                        Some(id) => {
                            // Inlined Assign(id, ...) -> AssignedId(id)
                            parent_ids.push(id);
                        }
                    }
                }
                Assign(head, parent_len) => {
                    let parent_start = parent_ids.len() - parent_len;
                    let id = match self.vertex_id_with_max_group(&head, group).await? {
                        Some(id) => id,
                        None => {
                            let id = next_free_id;
                            next_free_id = next_free_id + 1;
                            tracing::trace!(target: "dag::assign", "assign {:?} = {:?}", &head, id);
                            covered_ids.push(id);
                            self.insert(id, head.as_ref()).await?;
                            let parents = &parent_ids[parent_start..];
                            outcome.push_edge(id, parents);
                            id
                        }
                    };
                    parent_ids.truncate(parent_start);
                    // Inlined AssignId(id);
                    parent_ids.push(id);
                }
                AssignedId(id) => {
                    parent_ids.push(id);
                }
            }
        }

        Ok(outcome)
    }
}

impl<T> IdMapAssignHead for T where T: IdConvert + IdMapWrite {}

/// Write operations for IdMap.
#[async_trait::async_trait]
pub trait IdMapWrite {
    async fn insert(&mut self, id: Id, name: &[u8]) -> Result<()>;
    async fn remove_non_master(&mut self) -> Result<()>;
    async fn need_rebuild_non_master(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use nonblocking::non_blocking_result as r;
    use tempfile::tempdir;

    use super::*;
    use crate::ops::Persist;
    use crate::ops::PrefixLookup;

    #[cfg(all(test, feature = "indexedlog-backend"))]
    #[test]
    fn test_basic_operations() {
        let dir = tempdir().unwrap();
        let mut map = IdMap::open(dir.path()).unwrap();
        let lock = map.lock().unwrap();
        map.reload(&lock).unwrap();
        assert_eq!(map.next_free_id(Group::MASTER).unwrap().0, 0);
        map.insert(Id(1), b"abc").unwrap();
        assert_eq!(map.next_free_id(Group::MASTER).unwrap().0, 2);
        map.insert(Id(2), b"def").unwrap();
        assert_eq!(map.next_free_id(Group::MASTER).unwrap().0, 3);
        map.insert(Id(10), b"ghi").unwrap();
        assert_eq!(map.next_free_id(Group::MASTER).unwrap().0, 11);
        map.insert(Id(11), b"ghi").unwrap_err(); // ghi maps to 10
        map.insert(Id(10), b"ghi2").unwrap_err(); // 10 maps to ghi

        // Test another group.
        let id = map.next_free_id(Group::NON_MASTER).unwrap();
        map.insert(id, b"jkl").unwrap();
        map.insert(id, b"jkl").unwrap();
        map.insert(id, b"jkl2").unwrap_err(); // id maps to jkl
        map.insert(id + 1, b"jkl2").unwrap();
        map.insert(id + 2, b"jkl2").unwrap_err(); // jkl2 maps to id + 1
        map.insert(Id(15), b"jkl2").unwrap(); // reassign jkl2 to master group - ok.
        map.insert(id + 3, b"abc").unwrap_err(); // reassign abc to non-master group - error.
        assert_eq!(map.next_free_id(Group::NON_MASTER).unwrap(), id + 2);

        // Test hex lookup.
        assert_eq!(0x6a, b'j');
        assert_eq!(
            r(map.vertexes_by_hex_prefix(b"6a", 3)).unwrap(),
            [
                VertexName::from(&b"jkl"[..]),
                VertexName::from(&b"jkl2"[..])
            ]
        );
        assert_eq!(
            r(map.vertexes_by_hex_prefix(b"6a", 1)).unwrap(),
            [VertexName::from(&b"jkl"[..])]
        );
        assert!(r(map.vertexes_by_hex_prefix(b"6b", 1)).unwrap().is_empty());

        for _ in 0..=1 {
            assert_eq!(map.find_name_by_id(Id(1)).unwrap().unwrap(), b"abc");
            assert_eq!(map.find_name_by_id(Id(2)).unwrap().unwrap(), b"def");
            assert!(map.find_name_by_id(Id(3)).unwrap().is_none());
            assert_eq!(map.find_name_by_id(Id(10)).unwrap().unwrap(), b"ghi");

            assert_eq!(map.find_id_by_name(b"abc").unwrap().unwrap().0, 1);
            assert_eq!(map.find_id_by_name(b"def").unwrap().unwrap().0, 2);
            assert_eq!(map.find_id_by_name(b"ghi").unwrap().unwrap().0, 10);
            assert_eq!(map.find_id_by_name(b"jkl").unwrap().unwrap(), id);
            assert_eq!(map.find_id_by_name(b"jkl2").unwrap().unwrap().0, 15);
            assert!(map.find_id_by_name(b"jkl3").unwrap().is_none());
            // Error: re-assigned ids prevent sync.
            map.persist(&lock).unwrap_err();
        }

        // Test Debug
        assert_eq!(
            format!("{:?}", &map),
            r#"IdMap {
  abc: 1,
  def: 2,
  ghi: 10,
  jkl: N0,
  jkl2: N1,
  jkl2: 15,
}
"#
        );
    }
}
