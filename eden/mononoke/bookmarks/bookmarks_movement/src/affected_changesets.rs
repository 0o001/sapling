/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use anyhow::{anyhow, Context, Error, Result};
use blobrepo::BlobRepo;
use blobstore::Loadable;
use bookmarks::BookmarkUpdateReason;
use bookmarks_types::BookmarkName;
use bytes::Bytes;
use chrono::Utc;
use context::CoreContext;
use derived_data::{BonsaiDerivable, BonsaiDerived};
use futures::compat::Stream01CompatExt;
use futures::stream::{self, StreamExt, TryStreamExt};
use futures::{
    future::{self, try_join},
    stream::FuturesUnordered,
};
use hooks::{CrossRepoPushSource, HookManager};
use metaconfig_types::{BookmarkAttrs, InfinitepushParams, PushrebaseParams};
use mononoke_types::{BonsaiChangeset, ChangesetId};
use reachabilityindex::LeastCommonAncestorsHint;
use revset::DifferenceOfUnionsOfAncestorsNodeStream;
use scribe_commit_queue::{self, LogToScribe};
use skeleton_manifest::RootSkeletonManifestId;
use tunables::tunables;

use crate::hook_running::run_hooks;
use crate::restrictions::{BookmarkKind, BookmarkMoveAuthorization};
use crate::BookmarkMovementError;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum AdditionalChangesets {
    None,
    Ancestors(ChangesetId),
    Range {
        head: ChangesetId,
        base: ChangesetId,
    },
}

pub(crate) struct AffectedChangesets {
    /// Changesets that are being added to the repository and to this bookmark.
    new_changesets: HashMap<ChangesetId, BonsaiChangeset>,

    /// Changesets that are being used as a source for pushrebase.
    source_changesets: HashSet<BonsaiChangeset>,

    /// Additional changesets, if they have been loaded.
    additional_changesets: Option<HashSet<BonsaiChangeset>>,
}

impl AffectedChangesets {
    pub(crate) fn new() -> Self {
        Self {
            new_changesets: HashMap::new(),
            source_changesets: HashSet::new(),
            additional_changesets: None,
        }
    }

    pub(crate) fn with_source_changesets(source_changesets: HashSet<BonsaiChangeset>) -> Self {
        Self {
            new_changesets: HashMap::new(),
            source_changesets,
            additional_changesets: None,
        }
    }

    pub(crate) fn add_new_changesets(
        &mut self,
        new_changesets: HashMap<ChangesetId, BonsaiChangeset>,
    ) {
        if self.new_changesets.is_empty() {
            self.new_changesets = new_changesets;
        } else {
            self.new_changesets.extend(new_changesets);
        }
    }

    pub(crate) fn new_changesets(&self) -> &HashMap<ChangesetId, BonsaiChangeset> {
        &self.new_changesets
    }

    pub(crate) fn source_changesets(&self) -> &HashSet<BonsaiChangeset> {
        &self.source_changesets
    }

    /// Load bonsais in the additional changeset range that are not already in
    /// `new_changesets` and are ancestors of `head` but not ancestors of `base`
    /// or any of the `hooks_skip_ancestors_of` bookmarks for the named
    /// bookmark.
    ///
    /// These are the additional bonsais that we need to run hooks on for
    /// bookmark moves.
    async fn load_additional_changesets(
        &mut self,
        ctx: &CoreContext,
        repo: &BlobRepo,
        lca_hint: &Arc<dyn LeastCommonAncestorsHint>,
        bookmark_attrs: &BookmarkAttrs,
        bookmark: &BookmarkName,
        additional_changesets: AdditionalChangesets,
    ) -> Result<(), Error> {
        if self.additional_changesets.is_some() {
            return Ok(());
        }

        let (head, base) = match additional_changesets {
            AdditionalChangesets::None => {
                self.additional_changesets = Some(HashSet::new());
                return Ok(());
            }
            AdditionalChangesets::Ancestors(head) => (head, None),
            AdditionalChangesets::Range { head, base } => (head, Some(base)),
        };

        let mut exclude_bookmarks: HashSet<_> = bookmark_attrs
            .select(bookmark)
            .map(|params| params.hooks_skip_ancestors_of.iter())
            .flatten()
            .cloned()
            .collect();
        exclude_bookmarks.remove(bookmark);

        let mut excludes: HashSet<_> = stream::iter(exclude_bookmarks)
            .map(|bookmark| repo.bookmarks().get(ctx.clone(), &bookmark))
            .buffered(100)
            .try_filter_map(|maybe_cs_id| async move { Ok(maybe_cs_id) })
            .try_collect()
            .await?;
        excludes.extend(base);

        let range = DifferenceOfUnionsOfAncestorsNodeStream::new_with_excludes(
            ctx.clone(),
            &repo.get_changeset_fetcher(),
            lca_hint.clone(),
            vec![head],
            excludes.into_iter().collect(),
        )
        .compat()
        .try_filter(|bcs_id| {
            let exists = self.new_changesets.contains_key(bcs_id);
            future::ready(!exists)
        });

        let limit = match tunables().get_hooks_additional_changesets_limit() {
            limit if limit > 0 => limit as usize,
            _ => std::usize::MAX,
        };

        let additional_changesets = if tunables().get_run_hooks_on_additional_changesets() {
            let bonsais = range
                .and_then({
                    let mut count = 0;
                    move |bcs_id| {
                        count += 1;
                        if count > limit {
                            future::ready(Err(anyhow!(
                                "hooks additional changesets limit reached at {}",
                                bcs_id
                            )))
                        } else {
                            future::ready(Ok(bcs_id))
                        }
                    }
                })
                .map(|res| async move {
                    match res {
                        Ok(bcs_id) => Ok(bcs_id.load(ctx, repo.blobstore()).await?),
                        Err(e) => Err(e),
                    }
                })
                .buffered(100)
                .try_collect::<HashSet<_>>()
                .await?;

            ctx.scuba()
                .clone()
                .add("hook_running_additional_changesets", bonsais.len())
                .log_with_msg("Running hooks for additional changesets", None);

            bonsais
        } else {
            // Logging-only mode.  Work out how many changesets we would have run
            // on, and whether the limit would have been reached.
            let count = range
                .take(limit)
                .try_fold(0usize, |acc, _| async move { Ok(acc + 1) })
                .await?;

            let mut scuba = ctx.scuba().clone();
            scuba.add("hook_running_additional_changesets", count);
            if count >= limit {
                scuba.add("hook_running_additional_changesets_limit_reached", true);
            }
            scuba.log_with_msg("Hook running skipping additional changesets", None);
            HashSet::new()
        };

        self.additional_changesets = Some(additional_changesets);
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.new_changesets.is_empty()
            && self.source_changesets.is_empty()
            && self
                .additional_changesets
                .as_ref()
                .map_or(true, HashSet::is_empty)
    }

    fn iter(&self) -> impl Iterator<Item = &BonsaiChangeset> + Clone {
        self.new_changesets
            .values()
            .chain(self.source_changesets.iter())
            .chain(self.additional_changesets.iter().flatten())
    }

    /// Check all applicable restrictions on the affected changesets.
    pub(crate) async fn check_restrictions(
        &mut self,
        ctx: &CoreContext,
        repo: &BlobRepo,
        lca_hint: &Arc<dyn LeastCommonAncestorsHint>,
        pushrebase_params: &PushrebaseParams,
        bookmark_attrs: &BookmarkAttrs,
        hook_manager: &HookManager,
        bookmark: &BookmarkName,
        pushvars: Option<&HashMap<String, Bytes>>,
        reason: BookmarkUpdateReason,
        kind: BookmarkKind,
        auth: &BookmarkMoveAuthorization<'_>,
        additional_changesets: AdditionalChangesets,
        cross_repo_push_source: CrossRepoPushSource,
    ) -> Result<(), BookmarkMovementError> {
        self.check_case_conflicts(
            ctx,
            repo,
            lca_hint,
            pushrebase_params,
            bookmark_attrs,
            bookmark,
            kind,
            additional_changesets,
        )
        .await?;

        self.check_hooks(
            ctx,
            repo,
            lca_hint,
            bookmark_attrs,
            hook_manager,
            bookmark,
            pushvars,
            reason,
            kind,
            auth,
            additional_changesets,
            cross_repo_push_source,
        )
        .await?;

        self.check_service_write_restrictions(
            ctx,
            repo,
            lca_hint,
            bookmark_attrs,
            bookmark,
            auth,
            additional_changesets,
        )
        .await?;

        Ok(())
    }

    /// If the push is to a public bookmark, and the casefolding check is
    /// enabled, check that no affected changeset has case conflicts.
    async fn check_case_conflicts(
        &mut self,
        ctx: &CoreContext,
        repo: &BlobRepo,
        lca_hint: &Arc<dyn LeastCommonAncestorsHint>,
        pushrebase_params: &PushrebaseParams,
        bookmark_attrs: &BookmarkAttrs,
        bookmark: &BookmarkName,
        kind: BookmarkKind,
        additional_changesets: AdditionalChangesets,
    ) -> Result<(), BookmarkMovementError> {
        if kind == BookmarkKind::Public
            && pushrebase_params.flags.casefolding_check
            && tunables().get_check_case_conflicts_on_bookmark_movement()
            && repo
                .get_derived_data_config()
                .is_enabled(RootSkeletonManifestId::NAME)
        {
            self.load_additional_changesets(
                ctx,
                repo,
                lca_hint,
                bookmark_attrs,
                bookmark,
                additional_changesets,
            )
            .await
            .context("Failed to load additional affected changesets")?;

            stream::iter(self.iter().map(Ok))
                .try_for_each_concurrent(100, |bcs| async move {
                    let bcs_id = bcs.get_changeset_id();
                    let sk_mf = RootSkeletonManifestId::derive(ctx, repo, bcs_id)
                        .await
                        .map_err(Error::from)?
                        .into_skeleton_manifest_id()
                        .load(ctx, repo.blobstore())
                        .await
                        .map_err(Error::from)?;
                    if sk_mf.has_case_conflicts() {
                        // We only reject a commit if it introduces new case
                        // conflicts compared to its parents.
                        let parents = stream::iter(bcs.parents().map(|parent_bcs_id| async move {
                            RootSkeletonManifestId::derive(ctx, repo, parent_bcs_id)
                                .await
                                .map_err(Error::from)?
                                .into_skeleton_manifest_id()
                                .load(ctx, repo.blobstore())
                                .await
                                .map_err(Error::from)
                        }))
                        .buffered(10)
                        .try_collect::<Vec<_>>()
                        .await?;

                        if let Some((path1, path2)) = sk_mf
                            .first_new_case_conflict(ctx, repo.blobstore(), parents)
                            .await?
                        {
                            return Err(BookmarkMovementError::CaseConflict {
                                changeset_id: bcs_id,
                                path1,
                                path2,
                            });
                        }
                    }
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    /// If this is a user-initiated update to a public bookmark, run the
    /// hooks against the affected changesets.
    async fn check_hooks(
        &mut self,
        ctx: &CoreContext,
        repo: &BlobRepo,
        lca_hint: &Arc<dyn LeastCommonAncestorsHint>,
        bookmark_attrs: &BookmarkAttrs,
        hook_manager: &HookManager,
        bookmark: &BookmarkName,
        pushvars: Option<&HashMap<String, Bytes>>,
        reason: BookmarkUpdateReason,
        kind: BookmarkKind,
        auth: &BookmarkMoveAuthorization<'_>,
        additional_changesets: AdditionalChangesets,
        cross_repo_push_source: CrossRepoPushSource,
    ) -> Result<(), BookmarkMovementError> {
        if auth == &BookmarkMoveAuthorization::User && kind == BookmarkKind::Public {
            if reason == BookmarkUpdateReason::Push && tunables().get_disable_hooks_on_plain_push()
            {
                // Skip running hooks for this plain push.
                return Ok(());
            }

            if hook_manager.hooks_exist_for_bookmark(bookmark) {
                self.load_additional_changesets(
                    ctx,
                    repo,
                    lca_hint,
                    bookmark_attrs,
                    bookmark,
                    additional_changesets,
                )
                .await
                .context("Failed to load additional affected changesets")?;

                if !self.is_empty() {
                    run_hooks(
                        ctx,
                        hook_manager,
                        bookmark,
                        self.iter(),
                        pushvars,
                        cross_repo_push_source,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// If this is service-initiated update to a bookmark, check the update's
    /// affected changesets satisfy the service write restrictions.
    async fn check_service_write_restrictions(
        &mut self,
        ctx: &CoreContext,
        repo: &BlobRepo,
        lca_hint: &Arc<dyn LeastCommonAncestorsHint>,
        bookmark_attrs: &BookmarkAttrs,
        bookmark: &BookmarkName,
        auth: &BookmarkMoveAuthorization<'_>,
        additional_changesets: AdditionalChangesets,
    ) -> Result<(), BookmarkMovementError> {
        if let BookmarkMoveAuthorization::Service(service_name, scs_params) = auth {
            if scs_params.service_write_all_paths_permitted(service_name) {
                return Ok(());
            }

            self.load_additional_changesets(
                ctx,
                repo,
                lca_hint,
                bookmark_attrs,
                bookmark,
                additional_changesets,
            )
            .await
            .context("Failed to load additional affected changesets")?;

            for cs in self.iter() {
                if let Err(path) = scs_params.service_write_paths_permitted(service_name, cs) {
                    return Err(BookmarkMovementError::PermissionDeniedServicePath {
                        service_name: service_name.clone(),
                        path: path.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

pub async fn find_draft_ancestors(
    ctx: &CoreContext,
    repo: &BlobRepo,
    to_cs_id: ChangesetId,
) -> Result<Vec<BonsaiChangeset>, Error> {
    ctx.scuba()
        .clone()
        .log_with_msg("Started finding draft ancestors", None);

    let phases = repo.get_phases();
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    let mut drafts = vec![];
    queue.push_back(to_cs_id);
    visited.insert(to_cs_id);

    while let Some(cs_id) = queue.pop_front() {
        let public = phases
            .get_public(ctx.clone(), vec![cs_id], false /*ephemeral_derive*/)
            .await?;

        if public.contains(&cs_id) {
            continue;
        }
        drafts.push(cs_id);

        let parents = repo
            .get_changeset_parents_by_bonsai(ctx.clone(), cs_id)
            .await?;
        for p in parents {
            if visited.insert(p) {
                queue.push_back(p);
            }
        }
    }

    let drafts = stream::iter(drafts)
        .map(Ok)
        .map_ok(|cs_id| async move { cs_id.load(&ctx, &repo.get_blobstore()).await })
        .try_buffer_unordered(100)
        .try_collect::<Vec<_>>()
        .await?;

    ctx.scuba()
        .clone()
        .log_with_msg("Found draft ancestors", Some(format!("{}", drafts.len())));
    Ok(drafts)
}

pub(crate) async fn log_bonsai_commits_to_scribe(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bookmark: Option<&BookmarkName>,
    commits_to_log: Vec<BonsaiChangeset>,
    kind: BookmarkKind,
    infinitepush_params: &InfinitepushParams,
    pushrebase_params: &PushrebaseParams,
) {
    let mut new_changeset_ids_and_changed_files_count = Vec::new();
    for bcs in commits_to_log {
        let cs_id = bcs.get_changeset_id();
        let changed_files = bcs.file_changes_map().len();
        new_changeset_ids_and_changed_files_count.push((cs_id, changed_files));
    }

    let commit_scribe_category = match kind {
        BookmarkKind::Scratch => &infinitepush_params.commit_scribe_category,
        BookmarkKind::Public => &pushrebase_params.commit_scribe_category,
    };

    log_commits_to_scribe(
        ctx,
        repo,
        bookmark,
        new_changeset_ids_and_changed_files_count,
        commit_scribe_category.as_deref(),
    )
    .await;
}

pub async fn log_commits_to_scribe(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bookmark: Option<&BookmarkName>,
    changesets_and_changed_files_count: Vec<(ChangesetId, usize)>,
    commit_scribe_category: Option<&str>,
) {
    let queue = match commit_scribe_category {
        Some(category) if !category.is_empty() => {
            LogToScribe::new(ctx.scribe().clone(), category.to_string())
        }
        _ => LogToScribe::new_with_discard(),
    };

    let repo_id = repo.get_repoid();
    let bookmark = bookmark.map(|bm| bm.as_str());
    let received_timestamp = Utc::now();

    let futs: FuturesUnordered<_> = changesets_and_changed_files_count
        .into_iter()
        .map(|(changeset_id, changed_files_count)| {
            let queue = &queue;
            async move {
                let get_generation = async {
                    repo.get_generation_number(ctx.clone(), changeset_id)
                        .await?
                        .ok_or_else(|| Error::msg("No generation number found"))
                };
                let get_parents = async {
                    repo.get_changeset_parents_by_bonsai(ctx.clone(), changeset_id)
                        .await
                };

                let (generation, parents) = try_join(get_generation, get_parents).await?;

                let username = ctx.metadata().unix_name();
                let hostname = ctx.metadata().client_hostname();
                let identities = ctx.metadata().identities();
                let ci = scribe_commit_queue::CommitInfo::new(
                    repo_id,
                    bookmark,
                    generation,
                    changeset_id,
                    parents,
                    username.as_deref(),
                    identities,
                    hostname.as_deref(),
                    received_timestamp,
                    changed_files_count,
                );
                queue.queue_commit(&ci)
            }
        })
        .collect();
    let res = futs.try_for_each(|()| async { Ok(()) }).await;
    if let Err(err) = res {
        ctx.scuba()
            .clone()
            .log_with_msg("Failed to log pushed commits", Some(format!("{}", err)));
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use blobrepo_factory::new_memblob_empty;
    use fbinit::FacebookInit;
    use maplit::hashset;
    use std::collections::HashSet;
    use tests_utils::{bookmark, drawdag::create_from_dag};

    #[fbinit::compat_test]
    async fn test_find_draft_ancestors_simple(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let repo = new_memblob_empty(None)?;
        let mapping = create_from_dag(
            &ctx,
            &repo,
            r##"
            A-B-C-D
            "##,
        )
        .await?;

        let cs_id = mapping.get("A").unwrap();
        let to_cs_id = mapping.get("D").unwrap();
        bookmark(&ctx, &repo, "book").set_to(*cs_id).await?;
        let drafts = find_draft_ancestors(&ctx, &repo, *to_cs_id).await?;

        let drafts = drafts
            .into_iter()
            .map(|bcs| bcs.get_changeset_id())
            .collect::<HashSet<_>>();

        assert_eq!(
            drafts,
            hashset! {
                *mapping.get("B").unwrap(),
                *mapping.get("C").unwrap(),
                *mapping.get("D").unwrap(),
            }
        );

        bookmark(&ctx, &repo, "book")
            .set_to(*mapping.get("B").unwrap())
            .await?;
        let drafts = find_draft_ancestors(&ctx, &repo, *to_cs_id).await?;

        let drafts = drafts
            .into_iter()
            .map(|bcs| bcs.get_changeset_id())
            .collect::<HashSet<_>>();

        assert_eq!(
            drafts,
            hashset! {
                *mapping.get("C").unwrap(),
                *mapping.get("D").unwrap(),
            }
        );
        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_find_draft_ancestors_merge(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let repo = new_memblob_empty(None)?;
        let mapping = create_from_dag(
            &ctx,
            &repo,
            r##"
              B
             /  \
            A    D
             \  /
               C
            "##,
        )
        .await?;

        let cs_id = mapping.get("B").unwrap();
        let to_cs_id = mapping.get("D").unwrap();
        bookmark(&ctx, &repo, "book").set_to(*cs_id).await?;
        let drafts = find_draft_ancestors(&ctx, &repo, *to_cs_id).await?;

        let drafts = drafts
            .into_iter()
            .map(|bcs| bcs.get_changeset_id())
            .collect::<HashSet<_>>();

        assert_eq!(
            drafts,
            hashset! {
                *mapping.get("C").unwrap(),
                *mapping.get("D").unwrap(),
            }
        );

        Ok(())
    }
}
