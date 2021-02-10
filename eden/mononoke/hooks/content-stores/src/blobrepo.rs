/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Context as _};
use async_trait::async_trait;
use blobrepo::BlobRepo;
use blobrepo_hg::BlobRepoHg;
use blobstore::Loadable;
use bookmarks::BookmarkName;
use bytes::Bytes;
use context::CoreContext;
use futures::{future, stream::TryStreamExt};
use futures_util::future::TryFutureExt;
use manifest::{Diff, Entry, ManifestOps};
use mercurial_types::{FileType, HgFileNodeId, HgManifestId};
use mononoke_types::{ChangesetId, ContentId, MPath};
use std::collections::HashMap;

use crate::{ErrorKind, FileChange, FileContentManager, PathContent};

pub struct BlobRepoFileContentManager {
    pub repo: BlobRepo,
}

#[async_trait]
impl FileContentManager for BlobRepoFileContentManager {
    async fn get_file_size<'a>(
        &'a self,
        ctx: &'a CoreContext,
        id: ContentId,
    ) -> Result<u64, ErrorKind> {
        let store = self.repo.blobstore();
        Ok(filestore::get_metadata(store, ctx, &id.into())
            .await?
            .ok_or(ErrorKind::ContentIdNotFound(id))?
            .total_size)
    }

    async fn get_file_text<'a>(
        &'a self,
        ctx: &'a CoreContext,
        id: ContentId,
    ) -> Result<Option<Bytes>, ErrorKind> {
        let store = self.repo.blobstore();
        filestore::fetch_concat_opt(store, ctx, &id.into())
            .await?
            .ok_or(ErrorKind::ContentIdNotFound(id))
            .map(Option::Some)
    }

    async fn find_content<'a>(
        &'a self,
        ctx: &'a CoreContext,
        bookmark: BookmarkName,
        paths: Vec<MPath>,
    ) -> Result<HashMap<MPath, PathContent>, ErrorKind> {
        let changeset_id = self
            .repo
            .get_bonsai_bookmark(ctx.clone(), &bookmark)
            .await
            .with_context(|| format!("Error fetching bookmark: {}", bookmark))?
            .ok_or_else(|| format_err!("Bookmark {} does not exist", bookmark))?;

        let master_mf = derive_hg_manifest(ctx, &self.repo, changeset_id).await?;
        master_mf
            .find_entries(ctx.clone(), self.repo.get_blobstore(), paths)
            .map_ok(|(mb_path, entry)| async move {
                if let Some(path) = mb_path {
                    let content = resolve_content_id(ctx, &self.repo, entry).await?;
                    Ok(Some((path, content)))
                } else {
                    Ok(None)
                }
            })
            .try_buffer_unordered(100)
            .try_filter_map(future::ok)
            .try_collect::<HashMap<_, _>>()
            .map_err(ErrorKind::from)
            .await
    }

    async fn file_changes<'a>(
        &'a self,
        ctx: &'a CoreContext,
        new_cs_id: ChangesetId,
        old_cs_id: ChangesetId,
    ) -> Result<Vec<(MPath, FileChange)>, ErrorKind> {
        let new_mf_fut = derive_hg_manifest(ctx, &self.repo, new_cs_id);
        let old_mf_fut = derive_hg_manifest(ctx, &self.repo, old_cs_id);
        let (new_mf, old_mf) = future::try_join(new_mf_fut, old_mf_fut).await?;

        old_mf
            .diff(ctx.clone(), self.repo.get_blobstore(), new_mf)
            .map_err(ErrorKind::from)
            .map_ok(move |diff| async move {
                match diff {
                    Diff::Added(Some(path), entry) => {
                        match resolve_content_id(&ctx, &self.repo, entry).await? {
                            PathContent::File(content) => {
                                Ok(Some((path, FileChange::Added(content))))
                            }
                            PathContent::Directory => Ok(None),
                        }
                    }
                    Diff::Changed(Some(path), old_entry, entry) => {
                        let old_content = resolve_content_id(&ctx, &self.repo, old_entry);
                        let content = resolve_content_id(&ctx, &self.repo, entry);

                        match future::try_join(old_content, content).await? {
                            (PathContent::File(old_content_id), PathContent::File(content_id)) => {
                                Ok(Some((
                                    path,
                                    FileChange::Changed(old_content_id, content_id),
                                )))
                            }
                            _ => Ok(None),
                        }
                    }
                    Diff::Removed(Some(path), entry) => {
                        if let Entry::Leaf(_) = entry {
                            Ok(Some((path, FileChange::Removed)))
                        } else {
                            Ok(None)
                        }
                    }
                    _ => Ok(None),
                }
            })
            .try_buffer_unordered(100)
            .try_filter_map(future::ok)
            .try_collect()
            .await
    }
}

impl BlobRepoFileContentManager {
    pub fn new(repo: BlobRepo) -> BlobRepoFileContentManager {
        BlobRepoFileContentManager { repo }
    }
}

async fn derive_hg_manifest(
    ctx: &CoreContext,
    repo: &BlobRepo,
    changeset_id: ChangesetId,
) -> Result<HgManifestId, ErrorKind> {
    let hg_changeset_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), changeset_id)
        .await
        .with_context(|| format!("Error deriving hg changeset for bonsai: {}", changeset_id))?;
    let hg_mf_id = hg_changeset_id
        .load(&ctx, &repo.get_blobstore())
        .map_ok(|hg_changeset| hg_changeset.manifestid())
        .await
        .with_context(|| format!("Error loading hg changeset: {}", hg_changeset_id))?;

    Ok(hg_mf_id)
}

async fn resolve_content_id(
    ctx: &CoreContext,
    repo: &BlobRepo,
    entry: Entry<HgManifestId, (FileType, HgFileNodeId)>,
) -> Result<PathContent, ErrorKind> {
    match entry {
        Entry::Tree(_tree) => {
            // there is no content for trees
            Ok(PathContent::Directory)
        }
        Entry::Leaf((_type, file_node_id)) => file_node_id
            .load(ctx, &repo.get_blobstore())
            .map_ok(|file_env| PathContent::File(file_env.content_id()))
            .await
            .with_context(|| format!("Error loading filenode: {}", file_node_id))
            .map_err(ErrorKind::from),
    }
}
