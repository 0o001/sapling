/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::common::{
    find_source_config, find_target_bookmark_and_value, find_target_sync_config, MegarepoOp,
    SourceAndMovedChangesets,
};
use anyhow::anyhow;
use async_trait::async_trait;
use blobrepo::{save_bonsai_changesets, BlobRepo};
use blobstore::Loadable;
use commit_transformation::{
    create_directory_source_to_target_multi_mover, create_source_to_target_multi_mover,
    rewrite_commit, upload_commits,
};
use context::CoreContext;
use futures::{stream, StreamExt, TryStreamExt};
use megarepo_config::{
    MononokeMegarepoConfigs, Source, SourceMappingRules, SourceRevision, Target,
};
use megarepo_error::MegarepoError;
use megarepo_mapping::{CommitRemappingState, MegarepoMapping, SourceName};
use mononoke_api::Mononoke;
use mononoke_api::RepoContext;
use mononoke_types::{BonsaiChangeset, ChangesetId};
use mutable_renames::MutableRenames;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct SyncChangeset<'a> {
    megarepo_configs: &'a Arc<dyn MononokeMegarepoConfigs>,
    mononoke: &'a Arc<Mononoke>,
    target_megarepo_mapping: &'a Arc<MegarepoMapping>,
    mutable_renames: &'a Arc<MutableRenames>,
}

#[async_trait]
impl<'a> MegarepoOp for SyncChangeset<'a> {
    fn mononoke(&self) -> &Arc<Mononoke> {
        &self.mononoke
    }
}

const MERGE_COMMIT_MOVES_CONCURRENCY: usize = 10;

impl<'a> SyncChangeset<'a> {
    pub(crate) fn new(
        megarepo_configs: &'a Arc<dyn MononokeMegarepoConfigs>,
        mononoke: &'a Arc<Mononoke>,
        target_megarepo_mapping: &'a Arc<MegarepoMapping>,
        mutable_renames: &'a Arc<MutableRenames>,
    ) -> Self {
        Self {
            megarepo_configs,
            mononoke,
            target_megarepo_mapping,
            mutable_renames,
        }
    }

    pub(crate) async fn sync(
        &self,
        ctx: &CoreContext,
        source_cs_id: ChangesetId,
        source_name: &SourceName,
        target: &Target,
        target_location: ChangesetId,
    ) -> Result<ChangesetId, MegarepoError> {
        let target_repo = self.find_repo_by_id(&ctx, target.repo_id).await?;

        // Now we need to find the target config version that was used to create the latest
        // target commit. This config version will be used to sync the new changeset
        let (_, actual_target_location) =
            find_target_bookmark_and_value(&ctx, &target_repo, &target).await?;

        if target_location != actual_target_location {
            // Check if previous call was successful and return result if so
            return self
                .check_if_this_method_has_already_succeeded(
                    ctx,
                    source_cs_id,
                    source_name,
                    (target_location, actual_target_location),
                    &target_repo,
                )
                .await;
        }

        let (commit_remapping_state, target_config) = find_target_sync_config(
            &ctx,
            target_repo.blob_repo(),
            target_location,
            &target,
            &self.megarepo_configs,
        )
        .await?;

        // Given the SyncTargetConfig, let's find config for the source
        // we are going to sync from
        let source_config = find_source_config(&source_name, &target_config)?;

        // Find source repo and changeset that we need to sync
        let source_repo = self.find_repo_by_id(&ctx, source_config.repo_id).await?;
        let source_cs = source_cs_id
            .load(&ctx, source_repo.blob_repo().blobstore())
            .await?;

        validate_can_sync_changeset(
            &ctx,
            &target,
            &source_cs,
            &commit_remapping_state,
            &source_repo,
            &source_config,
        )
        .await?;

        // In case of merge commits we need to add move commits on top of the
        // merged-in commits.
        let side_parents_move_commits = self
            .create_move_commits(
                &ctx,
                &target,
                &source_cs,
                &commit_remapping_state,
                &source_repo,
                &source_name,
                &source_config,
            )
            .await?;

        // Finally create a commit in the target and update the mapping.
        let source_cs_id = source_cs.get_changeset_id();
        let new_target_cs_id = sync_changeset_to_target(
            &ctx,
            &source_config.mapping,
            &source_name,
            source_repo.blob_repo(),
            source_cs,
            target_repo.blob_repo(),
            target_location,
            &target,
            commit_remapping_state,
            &side_parents_move_commits,
        )
        .await?;

        self.target_megarepo_mapping
            .insert_source_target_cs_mapping(
                &ctx,
                source_name,
                &target,
                source_cs_id,
                new_target_cs_id,
                &target_config.version,
            )
            .await?;

        // Move the bookmark and record latest synced source changeset
        self.move_bookmark_conditionally(
            ctx,
            target_repo.blob_repo(),
            target.bookmark.clone(),
            (target_location, new_target_cs_id),
        )
        .await?;

        Ok(new_target_cs_id)
    }

    // Creates move commits on top of the merge parents in the source that
    // hasn't already been synced to targets (all but one). These move commits
    // put all source files into a correct places in a target so the file
    // history is correct.
    async fn create_move_commits(
        &self,
        ctx: &CoreContext,
        target: &Target,
        source_cs: &BonsaiChangeset,
        commit_remapping_state: &CommitRemappingState,
        target_repo: &RepoContext,
        source_name: &SourceName,
        source: &Source,
    ) -> Result<Vec<SourceAndMovedChangesets>, MegarepoError> {
        let latest_synced_cs_id =
            find_latest_synced_cs_id(commit_remapping_state, &source_name, target)?;

        // All parents except the one that's already synced to the target
        let side_parents = source_cs.parents().filter(|p| *p != latest_synced_cs_id);
        let mover = create_source_to_target_multi_mover(source.mapping.clone())
            .map_err(MegarepoError::request)?;
        let directory_mover = create_directory_source_to_target_multi_mover(source.mapping.clone())
            .map_err(MegarepoError::request)?;
        let moved_commits = stream::iter(side_parents)
            .map(|parent| {
                self.create_single_move_commit(
                    ctx,
                    target_repo.blob_repo(),
                    parent.clone(),
                    &mover,
                    &directory_mover,
                    Default::default(),
                    &source_name,
                )
            })
            .buffer_unordered(MERGE_COMMIT_MOVES_CONCURRENCY)
            .try_collect::<Vec<_>>()
            .await?;

        save_bonsai_changesets(
            moved_commits.iter().map(|css| css.moved.clone()).collect(),
            ctx.clone(),
            target_repo.blob_repo(),
        )
        .await?;

        let mutable_renames_count: usize = moved_commits
            .iter()
            .map(|css| css.mutable_renames.len())
            .sum();
        let mut scuba = ctx.scuba().clone();
        scuba.add("mutable_renames_count", mutable_renames_count);
        scuba.log_with_msg("Started saving mutable renames", None);
        self.save_mutable_renames(
            ctx,
            self.mutable_renames,
            moved_commits.iter().map(|css| &css.mutable_renames),
        )
        .await?;
        scuba.log_with_msg("Saved mutable renames", None);

        Ok(moved_commits)
    }

    // If that sync_changeset() call was successful, but failed to send
    // successful result to the client (e.g. network issues) then
    // client will retry a request. We need to detect this situation and
    // send a successful response to the client.
    async fn check_if_this_method_has_already_succeeded(
        &self,
        ctx: &CoreContext,
        source_cs_id: ChangesetId,
        source_name: &SourceName,
        (expected_target_location, actual_target_location): (ChangesetId, ChangesetId),
        repo: &RepoContext,
    ) -> Result<ChangesetId, MegarepoError> {
        // Bookmark points a non-expected commit - let's see if changeset it points to was created
        // by a previous sync_changeset call

        // Check that first parent is a target location
        let parents = repo
            .blob_repo()
            .get_changeset_parents_by_bonsai(ctx.clone(), actual_target_location)
            .await?;
        if parents.get(0) != Some(&expected_target_location) {
            return Err(MegarepoError::request(anyhow!(
                "Neither {} nor its first parent {:?} point to a target location {}",
                actual_target_location,
                parents.get(0),
                expected_target_location,
            )));
        }

        let state = self
            .read_remapping_state_file(ctx, repo, actual_target_location)
            .await?;

        let latest_synced = state.latest_synced_changesets.get(source_name);
        if Some(&source_cs_id) != latest_synced {
            return Err(MegarepoError::request(anyhow!(
                "In target commit {} latest synced source commit is {:?}, but expected {}",
                actual_target_location,
                latest_synced,
                source_cs_id,
            )));
        }

        Ok(actual_target_location)
    }
}

// We allow syncing changeset from a source if one of its parents was the latest synced changeset
// from this source into this target.
async fn validate_can_sync_changeset(
    _ctx: &CoreContext,
    target: &Target,
    source_cs: &BonsaiChangeset,
    commit_remapping_state: &CommitRemappingState,
    _source_repo: &RepoContext,
    source: &Source,
) -> Result<(), MegarepoError> {
    match &source.revision {
        SourceRevision::hash(_) => {
            /* If the revision is hardcoded hash it should be changed using remerge_source */
            return Err(MegarepoError::request(anyhow!(
                "can't sync changeset from source {} because this source points to a changeset",
                source.source_name,
            )));
        }
        SourceRevision::bookmark(_bookmark) => {
            /* If the source is following a git repo branch we can't verify much as the bookmark
            doesn't have to exist in the megarepo */
            ()
        }
        SourceRevision::UnknownField(_) => {
            return Err(MegarepoError::internal(anyhow!(
                "unexpected source revision!"
            )));
        }
    };

    let latest_synced_cs_id = find_latest_synced_cs_id(
        &commit_remapping_state,
        &SourceName::new(&source.source_name),
        target,
    )?;

    let found = source_cs.parents().find(|p| *p == latest_synced_cs_id);
    if found.is_none() {
        return Err(MegarepoError::request(anyhow!(
            "Can't sync {}, because latest synced commit is not a parent of this commit. \
                    Latest synced source changeset is {}",
            source_cs.get_changeset_id(),
            latest_synced_cs_id,
        )));
    }
    Ok(())
}

async fn sync_changeset_to_target(
    ctx: &CoreContext,
    mapping: &SourceMappingRules,
    source: &SourceName,
    source_repo: &BlobRepo,
    source_cs: BonsaiChangeset,
    target_repo: &BlobRepo,
    target_cs_id: ChangesetId,
    target: &Target,
    mut state: CommitRemappingState,
    side_parents_move_commits: &[SourceAndMovedChangesets],
) -> Result<ChangesetId, MegarepoError> {
    let mover =
        create_source_to_target_multi_mover(mapping.clone()).map_err(MegarepoError::internal)?;

    let source_cs_id = source_cs.get_changeset_id();
    // Create a new commit using a mover
    let source_cs_mut = source_cs.into_mut();
    let mut remapped_parents = HashMap::new();
    let latest_synced_cs_id = find_latest_synced_cs_id(&state, source, target)?;

    remapped_parents.insert(latest_synced_cs_id, target_cs_id);
    for css in side_parents_move_commits.iter() {
        remapped_parents.insert(css.source, css.moved.get_changeset_id());
    }

    let mut rewritten_commit = rewrite_commit(
        &ctx,
        source_cs_mut,
        &remapped_parents,
        mover,
        source_repo.clone(),
        // In case of octopus merges only first two parent get preserved during
        // hg derivation. This ensures that mainline is within those two so is
        // represented in the commit graph and the sync is a fast-forward move.
        Some(target_cs_id),
    )
    .await
    .map_err(MegarepoError::internal)?
    .ok_or_else(|| {
        MegarepoError::internal(anyhow!(
            "failed to rewrite commit {}, target: {:?}",
            source_cs_id,
            target
        ))
    })?;

    state.set_source_changeset(source.clone(), source_cs_id);
    state
        .save_in_changeset(ctx, target_repo, &mut rewritten_commit)
        .await?;

    let rewritten_commit = rewritten_commit.freeze().map_err(MegarepoError::internal)?;
    let target_cs_id = rewritten_commit.get_changeset_id();
    upload_commits(&ctx, vec![rewritten_commit], source_repo, target_repo)
        .await
        .map_err(MegarepoError::internal)?;

    Ok(target_cs_id)
}

fn find_latest_synced_cs_id(
    commit_remapping_state: &CommitRemappingState,
    source_name: &SourceName,
    target: &Target,
) -> Result<ChangesetId, MegarepoError> {
    let maybe_latest_synced_cs_id =
        commit_remapping_state.get_latest_synced_changeset(&source_name);
    if let Some(latest_synced_cs_id) = maybe_latest_synced_cs_id {
        Ok(latest_synced_cs_id.clone())
    } else {
        Err(MegarepoError::internal(anyhow!(
            "Source {:?} was not synced into target {:?}",
            source_name,
            target
        )))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::megarepo_test_utils::{MegarepoTest, SyncTargetConfigBuilder};
    use anyhow::Error;
    use fbinit::FacebookInit;
    use maplit::hashmap;
    use megarepo_mapping::REMAPPING_STATE_FILE;
    use mononoke_types::{FileChange, MPath};
    use tests_utils::{bookmark, list_working_copy_utf8, resolve_cs_id, CreateCommitContext};

    #[fbinit::test]
    async fn test_sync_changeset_simple(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let mut test = MegarepoTest::new(&ctx).await?;
        let target: Target = test.target("target".to_string());

        let source_name = SourceName::new("source_1");
        let version = "version_1".to_string();
        SyncTargetConfigBuilder::new(test.repo_id(), target.clone(), version.clone())
            .source_builder(source_name.clone())
            .set_prefix_bookmark_to_source_name()
            .build_source()?
            .build(&mut test.configs_storage);

        println!("Create initial source commit and bookmark");
        let init_source_cs_id = CreateCommitContext::new_root(&ctx, &test.blobrepo)
            .add_file("file", "content")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(init_source_cs_id)
            .await?;

        let latest_target_cs_id = test
            .prepare_initial_commit_in_target(&ctx, &version, &target)
            .await?;

        let configs_storage: Arc<dyn MononokeMegarepoConfigs> = Arc::new(test.configs_storage);
        let sync_changeset = SyncChangeset::new(
            &configs_storage,
            &test.mononoke,
            &test.megarepo_mapping,
            &test.mutable_renames,
        );
        println!("Trying to sync already synced commit again");
        let res = sync_changeset
            .sync(
                &ctx,
                init_source_cs_id,
                &source_name,
                &target,
                latest_target_cs_id,
            )
            .await;
        assert!(res.is_err());

        let source_cs_id = CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source_cs_id])
            .add_file("anotherfile", "anothercontent")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(source_cs_id)
            .await?;

        println!("Syncing new commit");
        sync_changeset
            .sync(
                &ctx,
                source_cs_id,
                &source_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        let cs_id = resolve_cs_id(&ctx, &test.blobrepo, "target").await?;
        let mut wc = list_working_copy_utf8(&ctx, &test.blobrepo, cs_id).await?;

        // Remove file with commit remapping state because it's never present in source
        wc.remove(&MPath::new(REMAPPING_STATE_FILE)?);

        assert_eq!(
            wc,
            hashmap! {
                MPath::new("source_1/file")? => "content".to_string(),
                MPath::new("source_1/anotherfile")? => "anothercontent".to_string(),
            }
        );

        Ok(())
    }

    #[fbinit::test]
    async fn test_sync_changeset_octopus_merge(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let mut test = MegarepoTest::new(&ctx).await?;
        let target: Target = test.target("target".to_string());

        let source_name = SourceName::new("source_1");
        let version = "version_1".to_string();
        SyncTargetConfigBuilder::new(test.repo_id(), target.clone(), version.clone())
            .source_builder(source_name.clone())
            .set_prefix_bookmark_to_source_name()
            .copyfile("file", "copyfile")
            .build_source()?
            .build(&mut test.configs_storage);

        println!("Create initial source commit and bookmark");
        let init_source_cs_id = CreateCommitContext::new_root(&ctx, &test.blobrepo)
            .add_file("file", "content")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(init_source_cs_id)
            .await?;

        let latest_target_cs_id = test
            .prepare_initial_commit_in_target(&ctx, &version, &target)
            .await?;

        let configs_storage: Arc<dyn MononokeMegarepoConfigs> = Arc::new(test.configs_storage);
        let sync_changeset = SyncChangeset::new(
            &configs_storage,
            &test.mononoke,
            &test.megarepo_mapping,
            &test.mutable_renames,
        );

        let merge_parent_1_source =
            CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source_cs_id])
                .add_file("file", "anothercontent")
                .add_file("file_from_parent_1", "parent_1")
                .commit()
                .await?;

        let merge_parent_2_source =
            CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source_cs_id])
                .add_file("file", "totallydifferentcontent")
                .add_file("file_from_parent_2", "parent_2")
                .commit()
                .await?;

        let merge_parent_3_source =
            CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source_cs_id])
                .add_file("file", "contentfromthirdparent")
                .add_file("file_from_parent_3", "parent_3")
                .commit()
                .await?;

        let merge_source = CreateCommitContext::new(
            &ctx,
            &test.blobrepo,
            vec![
                merge_parent_2_source,
                merge_parent_3_source,
                // Commit parent comming from the target last to ensure that
                // parent reordering works as expected.
                merge_parent_1_source,
            ],
        )
        .add_file("file", "mergeresolution")
        .add_file_with_copy_info(
            "copy_of_file",
            "totallydifferentcontent",
            (merge_parent_2_source, "file"),
        )
        .commit()
        .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(merge_parent_1_source)
            .await?;
        println!("Syncing first merge parent");
        let merge_parent_1_target = sync_changeset
            .sync(
                &ctx,
                merge_parent_1_source,
                &source_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(merge_source)
            .await?;
        println!("Syncing merge commit parent");
        let merge_target = sync_changeset
            .sync(
                &ctx,
                merge_source,
                &source_name,
                &target,
                merge_parent_1_target,
            )
            .await?;

        let mut wc = list_working_copy_utf8(&ctx, &test.blobrepo, merge_target).await?;

        // Remove file with commit remapping state because it's never present in source
        wc.remove(&MPath::new(REMAPPING_STATE_FILE)?);

        assert_eq!(
            wc,
            hashmap! {
                MPath::new("source_1/file")? => "mergeresolution".to_string(),
                MPath::new("source_1/file_from_parent_1")? => "parent_1".to_string(),
                MPath::new("source_1/file_from_parent_2")? => "parent_2".to_string(),
                MPath::new("source_1/file_from_parent_3")? => "parent_3".to_string(),
                MPath::new("source_1/copy_of_file")? => "totallydifferentcontent".to_string(),
                MPath::new("copyfile")? => "mergeresolution".to_string(),
            }
        );

        let merge_target_cs = merge_target.load(&ctx, &test.blobrepo.blobstore()).await?;

        let copied_file_change_from_bonsai = match merge_target_cs
            .file_changes()
            .find(|(p, _)| p == &&MPath::new("source_1/copy_of_file").unwrap())
            .unwrap()
            .1
        {
            FileChange::Change(tc) => tc,
            _ => panic!(),
        };
        assert_eq!(
            copied_file_change_from_bonsai.copy_from().unwrap().0,
            MPath::new("source_1/file")?
        );

        // All parents are preserved.
        assert_eq!(merge_target_cs.parents().count(), 3);

        // The parent from target comes first.
        assert_eq!(
            merge_target_cs.parents().next().unwrap(),
            merge_parent_1_target,
        );
        Ok(())
    }

    #[fbinit::test]
    async fn test_sync_changeset_two_sources_one_with_diamond_merge(
        fb: FacebookInit,
    ) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let mut test = MegarepoTest::new(&ctx).await?;
        let target: Target = test.target("target".to_string());

        let source1_name = SourceName::new("source_1");
        let source2_name = SourceName::new("source_2");
        let version = "version_1".to_string();
        SyncTargetConfigBuilder::new(test.repo_id(), target.clone(), version.clone())
            .source_builder(source1_name.clone())
            .set_prefix_bookmark_to_source_name()
            .build_source()?
            .source_builder(source2_name.clone())
            .set_prefix_bookmark_to_source_name()
            .build_source()?
            .build(&mut test.configs_storage);

        println!("Create initial first source commit and bookmark");
        let init_source1_cs_id = CreateCommitContext::new_root(&ctx, &test.blobrepo)
            .add_file("file1", "content1")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source1_name.to_string())
            .set_to(init_source1_cs_id)
            .await?;

        println!("Create initial second source commit and bookmark");
        let init_source2_cs_id = CreateCommitContext::new_root(&ctx, &test.blobrepo)
            .add_file("file2", "content2")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source2_name.to_string())
            .set_to(init_source2_cs_id)
            .await?;

        let mut latest_target_cs_id = test
            .prepare_initial_commit_in_target(&ctx, &version, &target)
            .await?;

        print!("Syncing one commit to each of sources... 1");
        let configs_storage: Arc<dyn MononokeMegarepoConfigs> = Arc::new(test.configs_storage);
        let sync_changeset = SyncChangeset::new(
            &configs_storage,
            &test.mononoke,
            &test.megarepo_mapping,
            &test.mutable_renames,
        );
        let source1_cs_id =
            CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source1_cs_id])
                .add_file("anotherfile1", "anothercontent")
                .commit()
                .await?;
        bookmark(&ctx, &test.blobrepo, source1_name.to_string())
            .set_to(source1_cs_id)
            .await?;
        latest_target_cs_id = sync_changeset
            .sync(
                &ctx,
                source1_cs_id,
                &source1_name,
                &target,
                latest_target_cs_id,
            )
            .await?;
        println!(", 2");

        let source2_cs_id =
            CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source2_cs_id])
                .add_file("anotherfile2", "anothercontent")
                .commit()
                .await?;
        bookmark(&ctx, &test.blobrepo, source2_name.to_string())
            .set_to(source2_cs_id)
            .await?;
        latest_target_cs_id = sync_changeset
            .sync(
                &ctx,
                source2_cs_id,
                &source2_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        println!("Trying to sync already synced commit again");
        let res = sync_changeset
            .sync(
                &ctx,
                source1_cs_id,
                &source1_name,
                &target,
                latest_target_cs_id,
            )
            .await;
        assert!(res.is_err());
        println!("Trying to sync a diamond merge commit");

        let source1_diamond_merge_cs_id = CreateCommitContext::new(
            &ctx,
            &test.blobrepo,
            vec![source1_cs_id, init_source1_cs_id],
        )
        .add_file("anotherfile1", "content_from_diamond_merge")
        .commit()
        .await?;
        bookmark(&ctx, &test.blobrepo, source1_name.to_string())
            .set_to(source1_diamond_merge_cs_id)
            .await?;
        let _diamond_merge_synced = sync_changeset
            .sync(
                &ctx,
                source1_diamond_merge_cs_id,
                &source1_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        let target_cs_id = resolve_cs_id(&ctx, &test.blobrepo, "target").await?;
        let mut wc = list_working_copy_utf8(&ctx, &test.blobrepo, target_cs_id).await?;

        // Remove file with commit remapping state because it's never present in source
        wc.remove(&MPath::new(REMAPPING_STATE_FILE)?);

        assert_eq!(
            wc,
            hashmap! {
                MPath::new("source_1/file1")? => "content1".to_string(),
                MPath::new("source_1/anotherfile1")? => "content_from_diamond_merge".to_string(),
                MPath::new("source_2/file2")? => "content2".to_string(),
                MPath::new("source_2/anotherfile2")? => "anothercontent".to_string(),
            }
        );

        let target_cs = target_cs_id.load(&ctx, &test.blobrepo.blobstore()).await?;
        // All parents are preserved.
        assert_eq!(target_cs.parents().count(), 2);

        Ok(())
    }

    #[fbinit::test]
    async fn test_sync_changeset_repeat_same_request(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);
        let mut test = MegarepoTest::new(&ctx).await?;
        let target: Target = test.target("target".to_string());

        let source_name = SourceName::new("source_1");
        let version = "version_1".to_string();
        SyncTargetConfigBuilder::new(test.repo_id(), target.clone(), version.clone())
            .source_builder(source_name.clone())
            .set_prefix_bookmark_to_source_name()
            .build_source()?
            .build(&mut test.configs_storage);

        println!("Create initial source commit and bookmark");
        let init_source_cs_id = CreateCommitContext::new_root(&ctx, &test.blobrepo)
            .add_file("file", "content")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(init_source_cs_id)
            .await?;

        let latest_target_cs_id = test
            .prepare_initial_commit_in_target(&ctx, &version, &target)
            .await?;

        let configs_storage: Arc<dyn MononokeMegarepoConfigs> = Arc::new(test.configs_storage);
        let sync_changeset = SyncChangeset::new(
            &configs_storage,
            &test.mononoke,
            &test.megarepo_mapping,
            &test.mutable_renames,
        );

        let source_cs_id = CreateCommitContext::new(&ctx, &test.blobrepo, vec![init_source_cs_id])
            .add_file("anotherfile", "anothercontent")
            .commit()
            .await?;

        bookmark(&ctx, &test.blobrepo, source_name.to_string())
            .set_to(source_cs_id)
            .await?;

        println!("Syncing new commit");
        let res1 = sync_changeset
            .sync(
                &ctx,
                source_cs_id,
                &source_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        println!("Now syncing the same commit again - should succeed");
        let res2 = sync_changeset
            .sync(
                &ctx,
                source_cs_id,
                &source_name,
                &target,
                latest_target_cs_id,
            )
            .await?;

        assert_eq!(res1, res2);

        Ok(())
    }
}
