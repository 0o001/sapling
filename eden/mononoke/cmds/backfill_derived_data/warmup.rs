/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use blame::{fetch_file_full_content, BlameRoot};
use blobrepo::BlobRepo;
use blobstore::Loadable;
use context::CoreContext;
use deleted_files_manifest::RootDeletedManifestId;
use derived_data::{BonsaiDerivable, BonsaiDerived, BonsaiDerivedMapping};
use fastlog::{fetch_parent_root_unodes, RootFastlog};
use fsnodes::{prefetch_content_metadata, RootFsnodeId};
use futures::{
    future::{self, try_join, try_join3, try_join4, FutureExt},
    stream::{self, StreamExt, TryStreamExt},
    TryFutureExt,
};
use manifest::find_intersection_of_diffs;
use mononoke_types::{ChangesetId, FileUnodeId};
use std::{collections::HashSet, sync::Arc};
use unodes::{find_unode_rename_sources, RootUnodeManifestId};

/// Types of derived data for which prefetching content for changed files
/// migth speed up derivation.
const PREFETCH_CONTENT_TYPES: &[&str] = &[BlameRoot::NAME];
const PREFETCH_CONTENT_METADATA_TYPES: &[&str] = &[RootFsnodeId::NAME];
const PREFETCH_UNODE_TYPES: &[&str] = &[RootFastlog::NAME, RootDeletedManifestId::NAME];

pub(crate) async fn warmup(
    ctx: &CoreContext,
    repo: &BlobRepo,
    derived_data_type: &str,
    chunk: &Vec<ChangesetId>,
) -> Result<(), Error> {
    // Warmup bonsai changesets unconditionally because
    // most likely all derived data needs it. And they are cheap to warm up anyway

    let bcs_warmup = async move {
        stream::iter(chunk)
            .map(move |cs_id| Ok(async move { cs_id.load(ctx, repo.blobstore()).await }))
            .try_for_each_concurrent(100, |x| async {
                x.await?;
                Result::<_, Error>::Ok(())
            })
            .await
    };

    let content_warmup = async {
        if PREFETCH_CONTENT_TYPES.contains(&derived_data_type) {
            content_warmup(ctx, repo, chunk).await?
        }
        Ok(())
    };

    let metadata_warmup = async {
        if PREFETCH_CONTENT_METADATA_TYPES.contains(&derived_data_type) {
            content_metadata_warmup(ctx, repo, chunk).await?
        }
        Ok(())
    };

    let unode_warmup = async {
        if PREFETCH_UNODE_TYPES.contains(&derived_data_type) {
            unode_warmup(ctx, repo, chunk).await?
        }
        Ok(())
    };

    try_join4(bcs_warmup, content_warmup, metadata_warmup, unode_warmup).await?;

    Ok(())
}

async fn content_warmup(
    ctx: &CoreContext,
    repo: &BlobRepo,
    chunk: &Vec<ChangesetId>,
) -> Result<(), Error> {
    stream::iter(chunk)
        .map(move |csid| Ok(prefetch_content(ctx, repo, csid)))
        .try_for_each_concurrent(100, |_| async { Ok(()) })
        .await
}

async fn content_metadata_warmup(
    ctx: &CoreContext,
    repo: &BlobRepo,
    chunk: &Vec<ChangesetId>,
) -> Result<(), Error> {
    stream::iter(chunk)
        .map({
            |cs_id| async move {
                let bcs = cs_id.load(ctx, repo.blobstore()).await?;

                let mut content_ids = HashSet::new();
                for (_, maybe_file_change) in bcs.file_changes() {
                    if let Some(file_change) = maybe_file_change {
                        content_ids.insert(file_change.content_id());
                    }
                }
                prefetch_content_metadata(ctx, repo.blobstore(), content_ids).await?;

                Result::<_, Error>::Ok(())
            }
        })
        .map(Result::<_, Error>::Ok)
        .try_for_each_concurrent(100, |f| f)
        .await?;
    Ok(())
}

async fn unode_warmup(
    ctx: &CoreContext,
    repo: &BlobRepo,
    chunk: &Vec<ChangesetId>,
) -> Result<(), Error> {
    stream::iter(chunk)
        .map({
            |cs_id| {
                async move {
                    let bcs = cs_id.load(ctx, repo.blobstore()).await?;

                    let root_mf_id =
                        RootUnodeManifestId::derive(&ctx, &repo, bcs.get_changeset_id())
                            .map_err(Error::from);

                    let parent_unodes = fetch_parent_root_unodes(ctx, repo, bcs);
                    let (root_mf_id, parent_unodes) = try_join(root_mf_id, parent_unodes).await?;
                    let unode_mf_id = root_mf_id.manifest_unode_id().clone();
                    find_intersection_of_diffs(
                        ctx.clone(),
                        Arc::new(repo.get_blobstore()),
                        unode_mf_id,
                        parent_unodes,
                    )
                    .try_for_each(|_| async { Ok(()) })
                    .await?;

                    Result::<_, Error>::Ok(())
                }
                .map(|_: Result<(), _>| ()) // Ignore warm up failures
            }
        })
        .for_each_concurrent(100, |f| f)
        .await;
    Ok(())
}

// Prefetch content of changed files between parents
async fn prefetch_content(
    ctx: &CoreContext,
    repo: &BlobRepo,
    csid: &ChangesetId,
) -> Result<(), Error> {
    async fn prefetch_content_unode(
        ctx: CoreContext,
        repo: BlobRepo,
        rename: Option<FileUnodeId>,
        file_unode_id: FileUnodeId,
    ) -> Result<(), Error> {
        let ctx = &ctx;
        let repo = &repo;
        let blobstore = repo.blobstore();
        let file_unode = file_unode_id.load(ctx, blobstore).await?;
        let options = BlameRoot::default_mapping(ctx, repo)?.options();
        let parents_content: Vec<_> = file_unode
            .parents()
            .iter()
            .cloned()
            .chain(rename)
            .map(|file_unode_id| fetch_file_full_content(ctx, repo, file_unode_id, options))
            .collect();

        // the assignment is needed to avoid unused_must_use warnings
        let _ = future::try_join(
            fetch_file_full_content(ctx, repo, file_unode_id, options),
            future::try_join_all(parents_content),
        )
        .await?;
        Ok(())
    }

    let bonsai = csid.load(ctx, repo.blobstore()).await?;

    let root_manifest_fut = RootUnodeManifestId::derive(ctx, &repo, csid.clone())
        .map_ok(|mf| mf.manifest_unode_id().clone())
        .map_err(Error::from);
    let parents_manifest_futs = bonsai.parents().collect::<Vec<_>>().into_iter().map({
        move |csid| {
            RootUnodeManifestId::derive(ctx, &repo, csid)
                .map_ok(|mf| mf.manifest_unode_id().clone())
                .map_err(Error::from)
        }
    });
    let (root_manifest, parents_manifests, renames) = try_join3(
        root_manifest_fut,
        future::try_join_all(parents_manifest_futs),
        find_unode_rename_sources(ctx, repo, &bonsai),
    )
    .await?;

    let blobstore = repo.get_blobstore().boxed();

    find_intersection_of_diffs(
        ctx.clone(),
        blobstore.clone(),
        root_manifest,
        parents_manifests,
    )
    .map_ok(|(path, entry)| Some((path?, entry.into_leaf()?)))
    .try_filter_map(|maybe_entry| async move { Result::<_, Error>::Ok(maybe_entry) })
    .map(|result| async {
        match result {
            Ok((path, file)) => {
                let rename_unode_id = renames.get(&path).map(|source| source.unode_id);
                let fut = prefetch_content_unode(ctx.clone(), repo.clone(), rename_unode_id, file);
                let join_handle = tokio::task::spawn(fut);
                join_handle.await?
            }
            Err(e) => Err(e),
        }
    })
    .buffered(256)
    .try_for_each(|()| future::ready(Ok(())))
    .await
}
