/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashSet;

use anyhow::{Context, Error, Result};
use cloned::cloned;
use futures::stream::{self, StreamExt, TryStream, TryStreamExt};

use blobstore::{Blobstore, Loadable};
use context::CoreContext;
use manifest::{bonsai_diff, BonsaiDiffFileChange, ManifestOps};
use mercurial_types::{
    blobs::{HgBlobChangeset, HgBlobEnvelope},
    HgFileEnvelope, HgFileNodeId, HgManifestId, MPath, RepoPath,
};
use mononoke_types::{
    BlobstoreValue, BonsaiChangeset, BonsaiChangesetMut, ChangesetId, FileChange, MononokeId,
};
use repo_blobstore::RepoBlobstore;
use sorted_vector_map::SortedVectorMap;

use crate::errors::*;
use crate::BlobRepo;

/// Creates bonsai changeset from already created HgBlobChangeset.
pub async fn create_bonsai_changeset_object(
    ctx: &CoreContext,
    cs: HgBlobChangeset,
    parent_manifests: Vec<HgManifestId>,
    bonsai_parents: Vec<ChangesetId>,
    repo: &BlobRepo,
) -> Result<BonsaiChangeset, Error> {
    let file_changes = find_file_changes(
        ctx,
        cs.clone(),
        parent_manifests,
        repo,
        bonsai_parents.clone(),
    )
    .await?;

    let extra = cs
        .extra()
        .iter()
        .map(|(key, value)| {
            // Extra keys must be valid UTF-8.   Mercurial supports arbitrary
            // bytes, but that is not supported in Mononoke.  Extra values can
            // be arbitrary bytes.
            let key = String::from_utf8(key.clone())?;
            Ok((key, value.clone()))
        })
        .collect::<Result<SortedVectorMap<_, _>, Error>>()?;

    let author = String::from_utf8(cs.user().to_vec())
        .with_context(|| format!("While converting author name {:?}", cs.user()))?;
    let message = String::from_utf8(cs.message().to_vec())
        .with_context(|| format!("While converting commit message {:?}", cs.message()))?;
    BonsaiChangesetMut {
        parents: bonsai_parents,
        author,
        author_date: *cs.time(),
        committer: None,
        committer_date: None,
        message,
        extra,
        file_changes,
        is_snapshot: false,
    }
    .freeze()
}

pub async fn save_bonsai_changeset_object(
    ctx: &CoreContext,
    blobstore: &RepoBlobstore,
    bonsai_cs: BonsaiChangeset,
) -> Result<(), Error> {
    let bonsai_blob = bonsai_cs.into_blob();
    let bcs_id = bonsai_blob.id().clone();
    let blobstore_key = bcs_id.blobstore_key();

    blobstore.put(ctx, blobstore_key, bonsai_blob.into()).await
}

fn find_bonsai_diff(
    ctx: &CoreContext,
    repo: &BlobRepo,
    cs: HgBlobChangeset,
    parent_manifests: HashSet<HgManifestId>,
) -> Result<impl TryStream<Ok = BonsaiDiffFileChange<HgFileNodeId>, Error = Error>> {
    Ok(bonsai_diff(
        ctx.clone(),
        repo.get_blobstore(),
        cs.manifestid(),
        parent_manifests,
    ))
}

// Finds files that were changed in the commit and returns it in the format suitable for BonsaiChangeset
async fn find_file_changes(
    ctx: &CoreContext,
    cs: HgBlobChangeset,
    parent_manifests: Vec<HgManifestId>,
    repo: &BlobRepo,
    bonsai_parents: Vec<ChangesetId>,
) -> Result<SortedVectorMap<MPath, FileChange>, Error> {
    let diff: Result<_, Error> =
        find_bonsai_diff(ctx, repo, cs, parent_manifests.iter().cloned().collect())
            .context("While finding bonsai diff")?
            .map_ok(|diff| {
                cloned!(parent_manifests, bonsai_parents);
                async move {
                    match diff {
                        BonsaiDiffFileChange::Changed(path, ty, entry_id) => {
                            let file_node_id = HgFileNodeId::new(entry_id.into_nodehash());
                            let envelope = file_node_id
                                .load(ctx, repo.blobstore())
                                .await
                                .context("While fetching bonsai file changes")?;
                            let size = envelope.content_size();
                            let content_id = envelope.content_id();

                            let copyinfo = get_copy_info(
                                ctx.clone(),
                                repo.clone(),
                                bonsai_parents,
                                path.clone(),
                                envelope,
                                parent_manifests,
                            )
                            .await
                            .context("While fetching copy information")?;
                            Ok((
                                path,
                                FileChange::tracked(content_id, ty, size as u64, copyinfo),
                            ))
                        }
                        BonsaiDiffFileChange::ChangedReusedId(path, ty, entry_id) => {
                            let file_node_id = HgFileNodeId::new(entry_id.into_nodehash());
                            let envelope = file_node_id
                                .load(ctx, repo.blobstore())
                                .await
                                .context("While fetching bonsai file changes")?;
                            let size = envelope.content_size();
                            let content_id = envelope.content_id();

                            // Reused ID means copy info is *not* stored.
                            Ok((path, FileChange::tracked(content_id, ty, size as u64, None)))
                        }
                        BonsaiDiffFileChange::Deleted(path) => Ok((path, FileChange::Deletion)),
                    }
                }
            })
            .try_buffer_unordered(100) // TODO(stash): magic number?
            .try_collect::<std::collections::BTreeMap<_, _>>()
            .await;

    Ok(SortedVectorMap::from_iter(diff?))
}

// Returns copy information for a given path and node if this file was copied.
// This function is quite complicated because hg and bonsai store copy information differently.
// In hg copy information is (path, filenode), in bonsai it's (path, parent cs id). That means that
// we need to find a parent from which this filenode was copied.
async fn get_copy_info(
    ctx: CoreContext,
    repo: BlobRepo,
    bonsai_parents: Vec<ChangesetId>,
    copy_from_path: MPath,
    envelope: HgFileEnvelope,
    parent_manifests: Vec<HgManifestId>,
) -> Result<Option<(MPath, ChangesetId)>, Error> {
    let node_id = envelope.node_id();

    let maybecopy = envelope
        .get_copy_info()?
        .map(|(path, hash)| (RepoPath::FilePath(path), hash));

    match maybecopy {
        Some((repopath, copyfromnode)) => {
            let repopath = repopath.mpath().ok_or(ErrorKind::UnexpectedRootPath)?;

            let parents_bonsai_and_mfs =
                stream::iter(bonsai_parents.into_iter().zip(parent_manifests.into_iter()));

            let get_bonsai_cs_copied_from =
                |(bonsai_parent, parent_mf): (ChangesetId, HgManifestId)| {
                    cloned!(ctx, repo);
                    async move {
                        let entry = parent_mf
                            .find_entry(ctx, repo.get_blobstore(), Some(repopath.clone()))
                            .await
                            .ok()?;
                        if entry?.into_leaf()?.1 == copyfromnode {
                            Some(bonsai_parent)
                        } else {
                            None
                        }
                    }
                };

            let copied_from = parents_bonsai_and_mfs
                .then(get_bonsai_cs_copied_from)
                .filter_map(|x| async move { x })
                .collect::<Vec<ChangesetId>>()
                .await;

            match copied_from.get(0) {
                Some(bonsai_cs_copied_from) => {
                    Ok(Some((repopath.clone(), bonsai_cs_copied_from.clone())))
                }
                None => Err(ErrorKind::IncorrectCopyInfo {
                    from_path: copy_from_path,
                    from_node: node_id,
                    to_path: repopath.clone(),
                    to_node: copyfromnode,
                }
                .into()),
            }
        }
        None => Ok(None),
    }
}
