// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

// NOTE: This isn't in `bonsai-utils` because blobrepo depends on it, while this depends on
// blobrepo.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use futures::{Future, Stream, future::{self, Either}};
use slog::Logger;

use futures_ext::{BoxFuture, FutureExt, StreamExt};

use blobrepo::{BlobManifest, BlobRepo, HgBlobChangeset, HgBlobEntry};
use blobrepo::internal::MemoryRootManifest;
use bonsai_utils::{bonsai_diff, BonsaiDiffResult};
use mercurial_types::{Changeset, Entry, HgChangesetId, HgManifestId, HgNodeHash, Type};
use mercurial_types::manifest_utils::{changed_entry_stream, ChangedEntry};
use mononoke_types::DateTime;

use changeset::{visit_changesets, ChangesetVisitMeta, ChangesetVisitor};
use errors::*;

#[derive(Clone, Debug)]
pub enum BonsaiVerifyResult {
    Valid {
        lookup_mf_id: HgNodeHash,
        computed_mf_id: HgNodeHash,
    },
    // ValidDifferentHash means that the root manifest ID didn't match up, but that that was
    // because of an expected difference in hash that isn't substantive.
    ValidDifferentId(BonsaiVerifyDifference),
    Invalid(BonsaiVerifyDifference),
    Ignored(HgChangesetId),
}

impl BonsaiVerifyResult {
    pub fn is_valid(&self) -> bool {
        match self {
            BonsaiVerifyResult::Valid { .. } | BonsaiVerifyResult::ValidDifferentId(..) => true,
            _ => false,
        }
    }

    pub fn is_ignored(&self) -> bool {
        match self {
            BonsaiVerifyResult::Ignored(..) => true,
            _ => false,
        }
    }
}

#[derive(Clone)]
pub struct BonsaiVerifyDifference {
    // Root manifests in treemanifest hybrid mode use a different ID than what's computed.
    // See the documentation in mercurial-types/if/mercurial_thrift.thrift's HgManifestEnvelope
    // for more.
    pub lookup_mf_id: HgNodeHash,
    // The difference/inconsistency is that expected_mf_id is not the same as roundtrip_mf_id.
    pub expected_mf_id: HgNodeHash,
    pub roundtrip_mf_id: HgNodeHash,
    repo: BlobRepo,
}

impl BonsaiVerifyDifference {
    /// What entries changed from the original manifest to the roundtripped one.
    pub fn changes(&self) -> impl Stream<Item = ChangedEntry, Error = Error> + Send {
        let original_mf = self.repo.get_manifest_by_nodeid(&self.lookup_mf_id);
        let roundtrip_mf = self.repo.get_manifest_by_nodeid(&self.roundtrip_mf_id);
        original_mf
            .join(roundtrip_mf)
            .map(|(original_mf, roundtrip_mf)| {
                changed_entry_stream(&roundtrip_mf, &original_mf, None)
            })
            .flatten_stream()
    }

    /// Whether there are any changes beyond the root manifest ID being different.
    #[inline]
    pub fn has_changes(&self) -> impl Future<Item = bool, Error = Error> + Send {
        self.changes().not_empty()
    }

    /// Whether there are any files that changed.
    #[inline]
    pub fn has_file_changes(&self) -> impl Future<Item = bool, Error = Error> + Send {
        self.changes()
            .filter(|item| !item.status.is_tree())
            .not_empty()
    }

    // XXX might need to return repo here if callers want to do direct queries
}

impl fmt::Debug for BonsaiVerifyDifference {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("BonsaiVerifyDifference")
            .field("lookup_mf_id", &format!("{}", self.lookup_mf_id))
            .field("expected_mf_id", &format!("{}", self.expected_mf_id))
            .field("roundtrip_mf_id", &format!("{}", self.roundtrip_mf_id))
            .finish()
    }
}

pub struct BonsaiVerify {
    pub logger: Logger,
    pub repo: BlobRepo,
    pub follow_limit: usize,
    pub ignores: HashSet<HgChangesetId>,
    pub broken_merges_before: Option<DateTime>,
    pub debug_bonsai_diff: bool,
}

impl BonsaiVerify {
    /// Verify that a list of changesets roundtrips through bonsai. Returns a stream of
    /// inconsistencies and errors encountered, which completes once verification is complete.
    pub fn verify(
        self,
        start_points: impl IntoIterator<Item = HgChangesetId>,
    ) -> impl Stream<Item = (BonsaiVerifyResult, ChangesetVisitMeta), Error = Error> + Send {
        let repo = self.repo.in_memory_writes_READ_DOC_COMMENT();

        visit_changesets(
            self.logger,
            repo,
            BonsaiVerifyVisitor {
                ignores: Arc::new(self.ignores),
                broken_merges_before: self.broken_merges_before,
                debug_bonsai_diff: self.debug_bonsai_diff,
            },
            start_points,
            self.follow_limit,
        )
    }
}

#[derive(Clone, Debug)]
struct BonsaiVerifyVisitor {
    ignores: Arc<HashSet<HgChangesetId>>,
    broken_merges_before: Option<DateTime>,
    debug_bonsai_diff: bool,
}

impl ChangesetVisitor for BonsaiVerifyVisitor {
    type Item = BonsaiVerifyResult;

    fn visit(
        self,
        logger: Logger,
        repo: BlobRepo,
        changeset: HgBlobChangeset,
        _follow_remaining: usize,
    ) -> BoxFuture<Self::Item, Error> {
        let changeset_id = changeset.get_changeset_id();
        if self.ignores.contains(&changeset_id) {
            debug!(logger, "Changeset ignored");
            return future::ok(BonsaiVerifyResult::Ignored(changeset_id)).boxify();
        }

        let broken_merge = match &self.broken_merges_before {
            Some(before) => {
                changeset.p1().is_some() && changeset.p2().is_some() && changeset.time() <= before
            }
            None => false,
        };

        if broken_merge {
            debug!(
                logger,
                "Potentially broken merge -- will check for file changes, not just manifest hash"
            );
        }

        debug!(logger, "Starting bonsai diff computation");

        let parents_fut = repo.get_changeset_parents(&changeset_id).and_then({
            let repo = repo.clone();
            move |parent_hashes| {
                let changesets = parent_hashes
                    .into_iter()
                    .map(move |parent_id| repo.get_changeset_by_changesetid(&parent_id));
                future::join_all(changesets)
            }
        });

        // Convert to bonsai first.
        let bonsai_diff_fut = parents_fut.and_then({
            let repo = repo.clone();
            move |parents| {
                let mut parents = parents.into_iter();
                let p1: Option<_> = parents.next();
                let p2: Option<_> = parents.next();

                let root_entry = get_root_entry(&repo, &changeset);
                let p1_entry = p1.map(|parent| get_root_entry(&repo, &parent));
                let p2_entry = p2.map(|parent| get_root_entry(&repo, &parent));
                let manifest_p1 = p1_entry
                    .as_ref()
                    .map(|entry| entry.get_hash().into_nodehash());
                let manifest_p2 = p2_entry
                    .as_ref()
                    .map(|entry| entry.get_hash().into_nodehash());

                // Also fetch the manifest as we're interested in the computed node id.
                let root_mf_id = HgManifestId::new(root_entry.get_hash().into_nodehash());
                let root_mf_fut = BlobManifest::load(&repo.get_blobstore(), &root_mf_id);

                bonsai_diff(root_entry, p1_entry, p2_entry)
                    .collect()
                    .join(root_mf_fut)
                    .and_then(move |(diff, root_mf)| match root_mf {
                        Some(root_mf) => Ok((diff, root_mf, manifest_p1, manifest_p2)),
                        None => bail_msg!(
                            "internal error: didn't find root manifest id {}",
                            root_mf_id
                        ),
                    })
            }
        });

        bonsai_diff_fut
            .and_then({
                let logger = logger.clone();
                move |(diff_result, root_mf, manifest_p1, manifest_p2)| {
                    let diff_count = diff_result.len();
                    debug!(
                        logger,
                        "Computed diff ({} entries), now applying it", diff_count,
                    );
                    if self.debug_bonsai_diff {
                        for diff in &diff_result {
                            debug!(logger, "diff result: {}", diff);
                        }
                    }

                    apply_diff(
                        logger.clone(),
                        repo.clone(),
                        diff_result,
                        manifest_p1.as_ref(),
                        manifest_p2.as_ref(),
                    ).and_then(move |roundtrip_mf_id| {
                        let lookup_mf_id = root_mf.node_id();
                        let computed_mf_id = root_mf.computed_node_id();
                        debug!(
                            logger,
                            "Saving complete: initial computed manifest ID: {} (original {}), \
                             roundtrip: {}",
                            computed_mf_id,
                            lookup_mf_id,
                            roundtrip_mf_id,
                        );

                        // If there's no diff, memory_manifest will return the same ID as the
                        // parent, which will be the lookup ID, not the computed one.
                        let expected_mf_id = if diff_count == 0 {
                            lookup_mf_id
                        } else {
                            computed_mf_id
                        };
                        if &roundtrip_mf_id == expected_mf_id {
                            Either::A(future::ok(BonsaiVerifyResult::Valid {
                                lookup_mf_id: *lookup_mf_id,
                                computed_mf_id: roundtrip_mf_id,
                            }))
                        } else {
                            let difference = BonsaiVerifyDifference {
                                lookup_mf_id: *lookup_mf_id,
                                expected_mf_id: *expected_mf_id,
                                roundtrip_mf_id,
                                repo,
                            };

                            if broken_merge {
                                // This is a (potentially) broken merge. Ignore tree changes and
                                // only check for file changes.
                                Either::B(Either::A(difference.has_file_changes().map(
                                    move |has_file_changes| {
                                        if has_file_changes {
                                            BonsaiVerifyResult::Invalid(difference)
                                        } else {
                                            BonsaiVerifyResult::ValidDifferentId(difference)
                                        }
                                    },
                                )))
                            } else if diff_count == 0 {
                                // This is an empty changeset. Mercurial is relatively inconsistent
                                // about creating new manifest nodes for such changesets, so it can
                                // happen.
                                Either::B(Either::B(difference.has_changes().map(
                                    move |has_changes| {
                                        if has_changes {
                                            BonsaiVerifyResult::Invalid(difference)
                                        } else {
                                            BonsaiVerifyResult::ValidDifferentId(difference)
                                        }
                                    },
                                )))
                            } else {
                                Either::A(future::ok(BonsaiVerifyResult::Invalid(difference)))
                            }
                        }
                    })
                }
            })
            .boxify()
    }
}

// This shouldn't actually be public, but it needs to be because of
// https://github.com/rust-lang/rust/issues/50865.
// TODO: (rain1) T31595868 make apply_diff private once Rust 1.29 is released
pub fn apply_diff(
    logger: Logger,
    repo: BlobRepo,
    diff_result: Vec<BonsaiDiffResult>,
    manifest_p1: Option<&HgNodeHash>,
    manifest_p2: Option<&HgNodeHash>,
) -> impl Future<Item = HgNodeHash, Error = Error> + Send {
    MemoryRootManifest::new(repo.clone(), manifest_p1, manifest_p2).and_then({
        move |memory_manifest| {
            let memory_manifest = Arc::new(memory_manifest);
            let futures: Vec<_> = diff_result
                .into_iter()
                .map(|result| {
                    let entry = make_entry(&repo, &result);
                    memory_manifest.change_entry(result.path(), entry)
                })
                .collect();

            future::join_all(futures)
                .and_then({
                    let memory_manifest = memory_manifest.clone();
                    move |_| memory_manifest.resolve_trivial_conflicts()
                })
                .and_then(move |_| {
                    // This will cause tree entries to be written to the blobstore, but
                    // those entries will be redirected to memory because of
                    // repo.in_memory_writes().
                    debug!(logger, "Applying complete: now saving");
                    memory_manifest.save()
                })
                .map(|m| m.get_hash().into_nodehash())
        }
    })
}

// XXX should this be in a more central place?
fn make_entry(repo: &BlobRepo, diff_result: &BonsaiDiffResult) -> Option<HgBlobEntry> {
    use self::BonsaiDiffResult::*;

    match diff_result {
        Changed(path, ft, entry_id) | ChangedReusedId(path, ft, entry_id) => {
            let blobstore = repo.get_blobstore();
            let basename = path.basename().clone();
            let hash = entry_id.into_nodehash();
            Some(HgBlobEntry::new(blobstore, basename, hash, Type::File(*ft)))
        }
        Deleted(_path) => None,
    }
}

#[inline]
fn get_root_entry(repo: &BlobRepo, changeset: &HgBlobChangeset) -> Box<Entry + Sync> {
    let manifest_id = changeset.manifestid();
    repo.get_root_entry(manifest_id)
}
