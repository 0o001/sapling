/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{anyhow, Error};
use blobstore::Loadable;
use context::CoreContext;
use cross_repo_sync::{CommitSyncContext, CommitSyncer};
use metaconfig_types::CommitSyncConfigVersion;
use mononoke_types::ChangesetId;
use std::collections::HashMap;
use synced_commit_mapping::SyncedCommitMapping;

/// This operation is useful immediately after a small repo is merged into a large repo.
/// See example below
///
///   B' <- manually synced commit from small repo (in small repo it is commit B)
///   |
///   BM <- "big merge"
///  /  \
/// ...  O <- big move commit i.e. commit that moves small repo files in correct location
///      |
///      A <- commit that was copied from small repo. It is identical between small and large repos.
///
/// Immediately after a small repo is merged into a large one we need to tell that a commit B and all of
/// its ancestors from small repo needs to be based on top of "big merge" commit in large repo rather than on top of
/// commit A.
/// The function below can be used to achieve exactly that.
pub async fn manual_commit_sync<M: SyncedCommitMapping + Clone + 'static>(
    ctx: &CoreContext,
    commit_syncer: &CommitSyncer<M>,
    source_cs_id: ChangesetId,
    target_repo_parents: Vec<ChangesetId>,
    mapping_version: CommitSyncConfigVersion,
) -> Result<Option<ChangesetId>, Error> {
    let source_repo = commit_syncer.get_source_repo();
    let source_cs = source_cs_id.load(ctx, source_repo.blobstore()).await?;
    let source_parents: Vec<_> = source_cs.parents().collect();
    if source_parents.len() != target_repo_parents.len() {
        return Err(anyhow!(
            "wrong number of parents: source repo has {} parents, while {} target repo parents specified",
            source_parents.len(),
            target_repo_parents.len(),
        ));
    }

    let remapped_parents = source_parents
        .into_iter()
        .zip(target_repo_parents.into_iter())
        .collect::<HashMap<_, _>>();

    let res = commit_syncer
        .unsafe_always_rewrite_sync_commit(
            ctx,
            source_cs_id,
            Some(remapped_parents),
            &mapping_version,
            CommitSyncContext::ManualCommitSync,
        )
        .await?;

    Ok(res)
}

#[cfg(test)]
mod test {
    use super::*;
    use cross_repo_sync_test_utils::init_small_large_repo;
    use fbinit::FacebookInit;
    use maplit::hashmap;
    use mononoke_types::MPath;
    use tests_utils::{list_working_copy_utf8, resolve_cs_id, CreateCommitContext};

    #[fbinit::compat_test]
    async fn test_manual_commit_sync(fb: FacebookInit) -> Result<(), Error> {
        let ctx = CoreContext::test_mock(fb);

        // Small and large repo look like that
        //
        // Small repo:
        //
        // O <- file3: "content3"
        // |
        // O <- file2: "content" <- "premove" bookmark, "megarepo_start" bookmark
        // |
        // O <- file: "content"
        //
        // Large repo
        // O <- file3: "content3"
        // |
        // O <- moves file -> prefix/file, file2 -> prefix/file2, "megarepo_start" bookmark
        // |
        // O <- file2: "content" <- "premove" bookmark
        // |
        // O <- file: "content"

        let (syncers, _) = init_small_large_repo(&ctx).await?;
        let small_to_large = syncers.small_to_large;
        let small_repo = small_to_large.get_source_repo();
        let large_repo = small_to_large.get_target_repo();

        // Create a commit on top of "premove" bookmark in a small repo, and then
        // manually sync it on top of big move bookmark.
        let premove_cs_id = resolve_cs_id(&ctx, &small_repo, "premove").await?;
        let commit_to_sync = CreateCommitContext::new(&ctx, &small_repo, vec![premove_cs_id])
            .add_file("some_other_file", "some_content")
            .commit()
            .await?;

        let bigmove = resolve_cs_id(&ctx, &large_repo, "megarepo_start").await?;

        let maybe_synced_commit = manual_commit_sync(
            &ctx,
            &small_to_large,
            commit_to_sync,
            vec![bigmove],
            small_to_large.get_current_version(&ctx)?,
        )
        .await?;

        let synced_commit = maybe_synced_commit.ok_or(anyhow!("commit was not synced"))?;
        let wc = list_working_copy_utf8(&ctx, &large_repo, synced_commit).await?;

        assert_eq!(
            hashmap! {
                MPath::new("prefix/file")? => "content".to_string(),
                MPath::new("prefix/file2")? => "content".to_string(),
                MPath::new("prefix/some_other_file")? => "some_content".to_string(),
            },
            wc
        );
        Ok(())
    }
}
