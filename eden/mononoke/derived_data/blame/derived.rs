/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Error};
use async_trait::async_trait;
use blobrepo::BlobRepo;
use blobstore::{Blobstore, BlobstoreBytes, Loadable};
use bytes::Bytes;
use cloned::cloned;
use context::CoreContext;
use derived_data::{BonsaiDerived, BonsaiDerivedMapping};
use filestore::{self, FetchKey};
use futures::{
    compat::Future01CompatExt,
    future::{FutureExt, TryFutureExt},
    StreamExt, TryStreamExt,
};
use futures_ext::{spawn_future, StreamExt as _};
use futures_old::{future, stream, Future, IntoFuture, Stream};
use manifest::find_intersection_of_diffs;
use mononoke_types::{
    blame::{store_blame, Blame, BlameId, BlameRejected},
    BonsaiChangeset, ChangesetId, ContentId, FileUnodeId, MPath,
};
use std::{collections::HashMap, iter::FromIterator, sync::Arc};
use thiserror::Error;
use unodes::{find_unode_renames, RootUnodeManifestId};

pub const BLAME_FILESIZE_LIMIT: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct BlameRoot(ChangesetId);

#[async_trait]
impl BonsaiDerived for BlameRoot {
    const NAME: &'static str = "blame";
    type Mapping = BlameRootMapping;

    fn mapping(_ctx: &CoreContext, repo: &BlobRepo) -> Self::Mapping {
        BlameRootMapping::new(repo.blobstore().boxed())
    }

    async fn derive_from_parents(
        ctx: CoreContext,
        repo: BlobRepo,
        bonsai: BonsaiChangeset,
        _parents: Vec<Self>,
    ) -> Result<Self, Error> {
        let csid = bonsai.get_changeset_id();
        let root_manifest = RootUnodeManifestId::derive(ctx.clone(), repo.clone(), csid)
            .from_err()
            .map(|mf| mf.manifest_unode_id().clone());
        let parents_manifest = bonsai
            .parents()
            .collect::<Vec<_>>() // iterator should be owned
            .into_iter()
            .map({
                cloned!(ctx, repo);
                move |csid| {
                    RootUnodeManifestId::derive(ctx.clone(), repo.clone(), csid)
                        .from_err()
                        .map(|mf| mf.manifest_unode_id().clone())
                }
            });

        (
            root_manifest,
            future::join_all(parents_manifest),
            find_unode_renames(ctx.clone(), repo.clone(), &bonsai),
        )
            .into_future()
            .and_then(move |(root_mf, parents_mf, renames)| {
                let renames = Arc::new(renames);
                let blobstore = repo.get_blobstore().boxed();
                find_intersection_of_diffs(ctx.clone(), blobstore.clone(), root_mf, parents_mf)
                    .boxed()
                    .compat()
                    .filter_map(|(path, entry)| Some((path?, entry.into_leaf()?)))
                    .map(move |(path, file)| {
                        spawn_future(create_blame(
                            ctx.clone(),
                            repo.clone(),
                            renames.clone(),
                            csid,
                            path,
                            file,
                        ))
                    })
                    .buffered(256)
                    .for_each(|_| Ok(()))
                    .map(move |_| BlameRoot(csid))
            })
            .compat()
            .await
    }
}

#[derive(Clone)]
pub struct BlameRootMapping {
    blobstore: Arc<dyn Blobstore>,
}

impl BlameRootMapping {
    pub fn new(blobstore: Arc<dyn Blobstore>) -> Self {
        Self { blobstore }
    }

    fn format_key(&self, csid: &ChangesetId) -> String {
        format!("derived_rootblame.v1.{}", csid)
    }
}

#[async_trait]
impl BonsaiDerivedMapping for BlameRootMapping {
    type Value = BlameRoot;

    async fn get(
        &self,
        ctx: CoreContext,
        csids: Vec<ChangesetId>,
    ) -> Result<HashMap<ChangesetId, Self::Value>, Error> {
        let futs = csids.into_iter().map(|csid| {
            self.blobstore
                .get(ctx.clone(), self.format_key(&csid))
                .compat()
                .map(move |val| val.map(|_| (csid.clone(), BlameRoot(csid))))
        });
        stream::FuturesUnordered::from_iter(futs)
            .filter_map(|v| v)
            .collect_to()
            .compat()
            .await
    }

    async fn put(
        &self,
        ctx: CoreContext,
        csid: ChangesetId,
        _id: Self::Value,
    ) -> Result<(), Error> {
        self.blobstore
            .put(
                ctx,
                self.format_key(&csid),
                BlobstoreBytes::from_bytes(Bytes::new()),
            )
            .await
    }
}

fn create_blame(
    ctx: CoreContext,
    repo: BlobRepo,
    renames: Arc<HashMap<MPath, FileUnodeId>>,
    csid: ChangesetId,
    path: MPath,
    file_unode_id: FileUnodeId,
) -> impl Future<Item = BlameId, Error = Error> {
    let blobstore = repo.blobstore().clone();

    file_unode_id
        .load(ctx.clone(), &blobstore)
        .compat()
        .from_err()
        .and_then(move |file_unode| {
            let parents_content_and_blame: Vec<_> = file_unode
                .parents()
                .iter()
                .cloned()
                .chain(renames.get(&path).cloned())
                .map({
                    cloned!(ctx, blobstore, repo);
                    move |file_unode_id| {
                        (
                            {
                                cloned!(ctx, repo);
                                async move {
                                    fetch_file_full_content(&ctx, &repo, file_unode_id).await
                                }
                            }
                            .boxed()
                            .compat(),
                            BlameId::from(file_unode_id)
                                .load(ctx.clone(), &blobstore)
                                .compat()
                                .from_err(),
                        )
                            .into_future()
                    }
                })
                .collect();

            (
                {
                    cloned!(ctx, repo);
                    async move { fetch_file_full_content(&ctx, &repo, file_unode_id).await }
                        .boxed()
                        .compat()
                },
                future::join_all(parents_content_and_blame),
            )
                .into_future()
                .and_then(move |(content, parents_content)| {
                    let blame_maybe_rejected = match content {
                        Err(rejected) => rejected.into(),
                        Ok(content) => {
                            let parents_content = parents_content
                                .into_iter()
                                .filter_map(|(content, blame_maybe_rejected)| {
                                    Some((content.ok()?, blame_maybe_rejected.into_blame().ok()?))
                                })
                                .collect();
                            Blame::from_parents(csid, content, path, parents_content)?.into()
                        }
                    };
                    Ok(blame_maybe_rejected)
                })
                .and_then(move |blame_maybe_rejected| {
                    store_blame(ctx, &blobstore, file_unode_id, blame_maybe_rejected)
                })
        })
}

pub async fn fetch_file_full_content(
    ctx: &CoreContext,
    repo: &BlobRepo,
    file_unode_id: FileUnodeId,
) -> Result<Result<Bytes, BlameRejected>, Error> {
    let blobstore = repo.blobstore();
    let file_unode = file_unode_id
        .load(ctx.clone(), blobstore)
        .map_err(|error| FetchError::Error(error.into()))
        .await?;

    let content_id = *file_unode.content_id();
    let result = fetch_from_filestore(ctx, repo, content_id).await;

    match result {
        Err(FetchError::Error(error)) => Err(error),
        Err(FetchError::Rejected(rejected)) => Ok(Err(rejected)),
        Ok(content) => Ok(Ok(content)),
    }
}

#[derive(Error, Debug)]
enum FetchError {
    #[error("FetchError::Rejected")]
    Rejected(#[source] BlameRejected),
    #[error("FetchError::Error")]
    Error(#[source] Error),
}

fn check_binary(content: &[u8]) -> Result<&[u8], FetchError> {
    if content.contains(&0u8) {
        Err(FetchError::Rejected(BlameRejected::Binary))
    } else {
        Ok(content)
    }
}

async fn fetch_from_filestore(
    ctx: &CoreContext,
    repo: &BlobRepo,
    content_id: ContentId,
) -> Result<Bytes, FetchError> {
    let blobstore = repo.blobstore();
    let result =
        filestore::fetch_with_size(blobstore, ctx.clone(), &FetchKey::Canonical(content_id))
            .map_err(FetchError::Error)
            .await?;

    match result {
        None => {
            let error = FetchError::Error(format_err!("Missing content: {}", content_id));
            Err(error)
        }
        Some((stream, size)) => {
            let config = repo.get_derived_data_config();
            let filesize_limit = config
                .override_blame_filesize_limit
                .unwrap_or(BLAME_FILESIZE_LIMIT);
            if size > filesize_limit {
                return Err(FetchError::Rejected(BlameRejected::TooBig));
            }
            let v = Vec::with_capacity(size as usize);
            let bytes = stream
                .map_err(FetchError::Error)
                .try_fold(v, |mut acc, bytes| async move {
                    acc.extend(check_binary(bytes.as_ref())?);
                    Ok(acc)
                })
                .map_ok(Bytes::from)
                .await?;
            Ok(bytes)
        }
    }
}
