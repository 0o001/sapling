/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use blobrepo::BlobRepo;
use blobrepo_hg::BlobRepoHg;
use blobstore::Loadable;
use context::CoreContext;
use derived_data::BonsaiDerived;
use futures::{future::try_join, TryStreamExt};
use manifest::{Diff, ManifestOps};
use mercurial_types::MPath;
use mononoke_types::ChangesetId;
use slog::info;
use unodes::RootUnodeManifestId;

pub async fn get_working_copy_paths(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bcs_id: ChangesetId,
) -> Result<Vec<MPath>, Error> {
    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), bcs_id)
        .await?;

    let hg_cs = hg_cs_id.load(ctx, repo.blobstore()).await?;
    info!(ctx.logger(), "Getting working copy contents");
    let mut paths: Vec<_> = hg_cs
        .manifestid()
        .list_leaf_entries(ctx.clone(), repo.get_blobstore())
        .map_ok(|(path, (_file_type, _filenode_id))| path)
        .try_collect()
        .await?;
    paths.sort();
    info!(ctx.logger(), "Done getting working copy contents");
    Ok(paths)
}

pub async fn get_changed_working_copy_paths(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bcs_id: ChangesetId,
    base_cs_id: ChangesetId,
) -> Result<Vec<MPath>, Error> {
    let unode_id = RootUnodeManifestId::derive(ctx, repo, bcs_id);
    let base_unode_id = RootUnodeManifestId::derive(ctx, repo, base_cs_id);

    let (unode_id, base_unode_id) = try_join(unode_id, base_unode_id).await?;

    let mut paths = base_unode_id
        .manifest_unode_id()
        .diff(
            ctx.clone(),
            repo.get_blobstore(),
            *unode_id.manifest_unode_id(),
        )
        .try_filter_map(|diff| async move {
            use Diff::*;
            let maybe_path = match diff {
                Added(maybe_path, entry) => entry.into_leaf().and_then(|_| maybe_path),
                Removed(_maybe_path, _entry) => None,
                Changed(maybe_path, _old_entry, new_entry) => {
                    new_entry.into_leaf().and_then(|_| maybe_path)
                }
            };

            Ok(maybe_path)
        })
        .try_collect::<Vec<_>>()
        .await?;

    paths.sort();
    Ok(paths)
}

pub async fn get_working_copy_paths_by_prefixes(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bcs_id: ChangesetId,
    prefixes: impl IntoIterator<Item = MPath>,
) -> Result<Vec<MPath>, Error> {
    let unode_id = RootUnodeManifestId::derive(ctx, repo, bcs_id).await?;
    let mut paths = unode_id
        .manifest_unode_id()
        .list_leaf_entries_under(ctx.clone(), repo.get_blobstore(), prefixes)
        .map_ok(|(mpath, _)| mpath)
        .try_collect::<Vec<MPath>>()
        .await?;
    paths.sort();
    Ok(paths)
}
