/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::derive::derive_unode_manifest;
use anyhow::{Error, Result};
use async_trait::async_trait;
use blobrepo::BlobRepo;
use blobstore::{Blobstore, BlobstoreGetData};
use bytes::Bytes;
use context::CoreContext;
use derived_data::{BonsaiDerived, BonsaiDerivedMapping};
use futures::compat::Future01CompatExt;
use futures::future::TryFutureExt;
use futures_ext::StreamExt;
use futures_old::{
    stream::{self, FuturesUnordered},
    Future, Stream,
};
use metaconfig_types::UnodeVersion;
use mononoke_types::{
    BlobstoreBytes, BonsaiChangeset, ChangesetId, ContentId, FileType, MPath, ManifestUnodeId,
};
use repo_blobstore::RepoBlobstore;
use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    iter::FromIterator,
};

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct RootUnodeManifestId(ManifestUnodeId);

impl RootUnodeManifestId {
    pub fn manifest_unode_id(&self) -> &ManifestUnodeId {
        &self.0
    }
}

impl TryFrom<BlobstoreBytes> for RootUnodeManifestId {
    type Error = Error;

    fn try_from(blob_bytes: BlobstoreBytes) -> Result<Self> {
        ManifestUnodeId::from_bytes(&blob_bytes.into_bytes()).map(RootUnodeManifestId)
    }
}

impl TryFrom<BlobstoreGetData> for RootUnodeManifestId {
    type Error = Error;

    fn try_from(blob_val: BlobstoreGetData) -> Result<Self> {
        blob_val.into_bytes().try_into()
    }
}

impl From<RootUnodeManifestId> for BlobstoreBytes {
    fn from(root_mf_id: RootUnodeManifestId) -> Self {
        BlobstoreBytes::from_bytes(Bytes::copy_from_slice(root_mf_id.0.blake2().as_ref()))
    }
}

#[async_trait]
impl BonsaiDerived for RootUnodeManifestId {
    const NAME: &'static str = "unodes";
    type Mapping = RootUnodeManifestMapping;

    fn mapping(_ctx: &CoreContext, repo: &BlobRepo) -> Self::Mapping {
        RootUnodeManifestMapping::new(
            repo.blobstore().clone(),
            repo.get_derived_data_config().unode_version,
        )
    }

    async fn derive_from_parents(
        ctx: CoreContext,
        repo: BlobRepo,
        bonsai: BonsaiChangeset,
        parents: Vec<Self>,
    ) -> Result<Self, Error> {
        let bcs_id = bonsai.get_changeset_id();
        derive_unode_manifest(
            ctx,
            repo,
            bcs_id,
            parents
                .into_iter()
                .map(|root_mf_id| root_mf_id.manifest_unode_id().clone())
                .collect(),
            get_file_changes(&bonsai),
        )
        .map(RootUnodeManifestId)
        .compat()
        .await
    }
}

// TODO(stash): have a generic version of blobstore derived data mapping?
#[derive(Clone)]
pub struct RootUnodeManifestMapping {
    blobstore: RepoBlobstore,
    unode_version: UnodeVersion,
}

impl RootUnodeManifestMapping {
    pub fn new(blobstore: RepoBlobstore, unode_version: UnodeVersion) -> Self {
        Self {
            blobstore,
            unode_version,
        }
    }

    fn format_key(&self, cs_id: ChangesetId) -> String {
        match self.unode_version {
            UnodeVersion::V1 => format!("derived_root_unode.{}", cs_id),
            UnodeVersion::V2 => format!("derived_root_unode_v2.{}", cs_id),
        }
    }

    fn fetch_unode(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> impl Future<Item = Option<(ChangesetId, RootUnodeManifestId)>, Error = Error> {
        self.blobstore
            .get(ctx.clone(), self.format_key(cs_id))
            .compat()
            .and_then(|maybe_bytes| maybe_bytes.map(|bytes| bytes.try_into()).transpose())
            .map(move |maybe_root_mf_id| maybe_root_mf_id.map(|root_mf_id| (cs_id, root_mf_id)))
    }
}

#[async_trait]
impl BonsaiDerivedMapping for RootUnodeManifestMapping {
    type Value = RootUnodeManifestId;

    async fn get(
        &self,
        ctx: CoreContext,
        csids: Vec<ChangesetId>,
    ) -> Result<HashMap<ChangesetId, Self::Value>, Error> {
        let gets = csids.into_iter().map(|cs_id| {
            self.fetch_unode(ctx.clone(), cs_id)
                .map(|maybe_root_mf_id| stream::iter_ok(maybe_root_mf_id.into_iter()))
        });
        FuturesUnordered::from_iter(gets)
            .flatten()
            .collect_to()
            .compat()
            .await
    }

    async fn put(&self, ctx: CoreContext, csid: ChangesetId, id: Self::Value) -> Result<(), Error> {
        self.blobstore
            .put(ctx, self.format_key(csid), id.into())
            .await
    }
}

pub(crate) fn get_file_changes(
    bcs: &BonsaiChangeset,
) -> Vec<(MPath, Option<(ContentId, FileType)>)> {
    bcs.file_changes()
        .map(|(mpath, maybe_file_change)| {
            let content_file_type = match maybe_file_change {
                Some(file_change) => Some((file_change.content_id(), file_change.file_type())),
                None => None,
            };
            (mpath.clone(), content_file_type)
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use blobrepo_hg::BlobRepoHg;
    use blobstore::Loadable;
    use bookmarks::BookmarkName;
    use cloned::cloned;
    use derived_data_test_utils::iterate_all_manifest_entries;
    use fbinit::FacebookInit;
    use fixtures::{
        branch_even, branch_uneven, branch_wide, linear, many_diamonds, many_files_dirs,
        merge_even, merge_uneven, unshared_merge_even, unshared_merge_uneven,
    };
    use futures::future::{Future as NewFuture, FutureExt, TryFutureExt};
    use futures::stream::TryStreamExt;
    use manifest::Entry;
    use mercurial_types::{HgChangesetId, HgManifestId};
    use revset::AncestorsNodeStream;
    use tokio_compat::runtime::Runtime;

    fn fetch_manifest_by_cs_id(
        ctx: CoreContext,
        repo: BlobRepo,
        hg_cs_id: HgChangesetId,
    ) -> impl Future<Item = HgManifestId, Error = Error> {
        hg_cs_id
            .load(ctx, repo.blobstore())
            .compat()
            .from_err()
            .map(|hg_cs| hg_cs.manifestid())
    }

    fn verify_unode(
        ctx: CoreContext,
        repo: BlobRepo,
        bcs_id: ChangesetId,
        hg_cs_id: HgChangesetId,
    ) -> impl Future<Item = (), Error = Error> {
        let unode_entries = RootUnodeManifestId::derive(ctx.clone(), repo.clone(), bcs_id)
            .from_err()
            .map(|root_mf_unode| root_mf_unode.manifest_unode_id().clone())
            .and_then({
                cloned!(ctx, repo);
                move |mf_unode_id| {
                    async move {
                        iterate_all_manifest_entries(&ctx, &repo, Entry::Tree(mf_unode_id))
                            .compat()
                            .map(|(path, _)| path)
                            .collect()
                            .map(|mut paths| {
                                paths.sort();
                                paths
                            })
                            .compat()
                            .await
                    }
                    .boxed()
                    .compat()
                }
            });

        let filenode_entries = fetch_manifest_by_cs_id(ctx.clone(), repo.clone(), hg_cs_id)
            .and_then({
                cloned!(ctx, repo);
                move |root_mf_id| {
                    async move {
                        iterate_all_manifest_entries(&ctx, &repo, Entry::Tree(root_mf_id))
                            .compat()
                            .map(|(path, _)| path)
                            .collect()
                            .map(|mut paths| {
                                paths.sort();
                                paths
                            })
                            .compat()
                            .await
                    }
                    .boxed()
                    .compat()
                }
            });

        unode_entries
            .join(filenode_entries)
            .map(|(unode_entries, filenode_entries)| {
                assert_eq!(unode_entries, filenode_entries);
            })
    }

    fn all_commits(
        ctx: CoreContext,
        repo: BlobRepo,
    ) -> impl Stream<Item = (ChangesetId, HgChangesetId), Error = Error> {
        let master_book = BookmarkName::new("master").unwrap();
        repo.get_bonsai_bookmark(ctx.clone(), &master_book)
            .map(move |maybe_bcs_id| {
                let bcs_id = maybe_bcs_id.unwrap();
                AncestorsNodeStream::new(ctx.clone(), &repo.get_changeset_fetcher(), bcs_id.clone())
                    .and_then(move |new_bcs_id| {
                        repo.get_hg_from_bonsai_changeset(ctx.clone(), new_bcs_id)
                            .map(move |hg_cs_id| (new_bcs_id, hg_cs_id))
                    })
            })
            .flatten_stream()
    }

    fn verify_repo<F>(fb: FacebookInit, repo: F, runtime: &mut Runtime)
    where
        F: NewFuture<Output = BlobRepo>,
    {
        let ctx = CoreContext::test_mock(fb);
        let repo = runtime.block_on_std(repo);

        runtime
            .block_on(
                all_commits(ctx.clone(), repo.clone())
                    .and_then(move |(bcs_id, hg_cs_id)| {
                        verify_unode(ctx.clone(), repo.clone(), bcs_id, hg_cs_id)
                    })
                    .collect(),
            )
            .unwrap();
    }

    #[fbinit::test]
    fn test_derive_data(fb: FacebookInit) {
        let mut runtime = Runtime::new().unwrap();
        verify_repo(fb, linear::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_even::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_uneven::getrepo(fb), &mut runtime);
        verify_repo(fb, branch_wide::getrepo(fb), &mut runtime);
        verify_repo(fb, many_diamonds::getrepo(fb), &mut runtime);
        verify_repo(fb, many_files_dirs::getrepo(fb), &mut runtime);
        verify_repo(fb, merge_even::getrepo(fb), &mut runtime);
        verify_repo(fb, merge_uneven::getrepo(fb), &mut runtime);
        verify_repo(fb, unshared_merge_even::getrepo(fb), &mut runtime);
        verify_repo(fb, unshared_merge_uneven::getrepo(fb), &mut runtime);
    }
}
