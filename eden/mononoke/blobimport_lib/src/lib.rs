/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

mod bookmark;
mod changeset;
mod concurrency;

use std::cmp;
use std::collections::HashMap;
use std::error::Error as StdError;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Error;
use ascii::AsciiString;
use cloned::cloned;
use futures_ext::{BoxFuture, FutureExt, StreamExt};
use futures_old::{future, Future, Stream};
use futures_preview::{
    compat::Future01CompatExt,
    future::{ready, Future as NewFuture, FutureExt as _, TryFutureExt},
    stream::futures_unordered::FuturesUnordered,
    TryStreamExt,
};
use slog::{debug, error, info, Logger};

use blobrepo::BlobRepo;
use bonsai_git_mapping::BonsaiGitMapping;
use bonsai_globalrev_mapping::{bulk_import_globalrevs, BonsaiGlobalrevMapping};
use context::CoreContext;
use derived_data_utils::derived_data_utils;
use mercurial_revlog::{revlog::RevIdx, RevlogRepo};
use mercurial_types::{HgChangesetId, HgNodeHash};
use mononoke_types::{ChangesetId, RepositoryId};
use synced_commit_mapping::{SyncedCommitMapping, SyncedCommitMappingEntry};

use crate::changeset::UploadChangesets;

fn derive_data_for_csids(
    ctx: CoreContext,
    repo: BlobRepo,
    csids: Vec<ChangesetId>,
    derived_data_types: &[String],
) -> Result<impl NewFuture<Output = Result<(), Error>>, Error> {
    let derivations = FuturesUnordered::new();

    for data_type in derived_data_types {
        let derived_utils = derived_data_utils(repo.clone(), data_type)?;

        derivations.push(
            derived_utils
                .derive_batch(ctx.clone(), repo.clone(), csids.clone())
                .map(|_| ())
                .compat(),
        );
    }

    Ok(async move {
        derivations.try_for_each(|_| ready(Ok(()))).await?;
        Ok(())
    })
}

// What to do with bookmarks when blobimporting a repo
pub enum BookmarkImportPolicy {
    // Do not import bookmarks
    Ignore,
    // Prefix bookmark names when importing
    Prefix(AsciiString),
}

pub struct Blobimport {
    pub ctx: CoreContext,
    pub logger: Logger,
    pub blobrepo: BlobRepo,
    pub revlogrepo_path: PathBuf,
    pub changeset: Option<HgNodeHash>,
    pub skip: Option<usize>,
    pub commits_limit: Option<usize>,
    pub bookmark_import_policy: BookmarkImportPolicy,
    pub globalrevs_store: Arc<dyn BonsaiGlobalrevMapping>,
    pub synced_commit_mapping: Arc<dyn SyncedCommitMapping>,
    pub lfs_helper: Option<String>,
    pub concurrent_changesets: usize,
    pub concurrent_blobs: usize,
    pub concurrent_lfs_imports: usize,
    pub fixed_parent_order: HashMap<HgChangesetId, Vec<HgChangesetId>>,
    pub has_globalrev: bool,
    pub populate_git_mapping: bool,
    pub small_repo_id: Option<RepositoryId>,
    pub derived_data_types: Vec<String>,
}

impl Blobimport {
    pub fn import(self) -> BoxFuture<Option<RevIdx>, Error> {
        let Self {
            ctx,
            logger,
            blobrepo,
            revlogrepo_path,
            changeset,
            skip,
            commits_limit,
            bookmark_import_policy,
            globalrevs_store,
            synced_commit_mapping,
            lfs_helper,
            concurrent_changesets,
            concurrent_blobs,
            concurrent_lfs_imports,
            fixed_parent_order,
            has_globalrev,
            populate_git_mapping,
            small_repo_id,
            derived_data_types,
        } = self;

        let repo_id = blobrepo.get_repoid();

        let stale_bookmarks = {
            let revlogrepo = RevlogRepo::open(&revlogrepo_path).expect("cannot open revlogrepo");
            bookmark::read_bookmarks(revlogrepo)
        };

        let revlogrepo = RevlogRepo::open(revlogrepo_path).expect("cannot open revlogrepo");

        let log_step = match commits_limit {
            Some(commits_limit) => cmp::max(1, commits_limit / 10),
            None => 5000,
        };

        let chunk_size = 100;

        let upload_changesets = UploadChangesets {
            ctx: ctx.clone(),
            blobrepo: blobrepo.clone(),
            revlogrepo: revlogrepo.clone(),
            changeset,
            skip,
            commits_limit,
            lfs_helper,
            concurrent_changesets,
            concurrent_blobs,
            concurrent_lfs_imports,
            fixed_parent_order,
        }
        .upload()
        .enumerate()
        .map({
            let logger = logger.clone();
            move |(cs_count, (revidx, cs))| {
                debug!(logger, "{} inserted: {}", cs_count, cs.1.get_changeset_id());
                if cs_count % log_step == 0 {
                    info!(logger, "inserted commits # {}", cs_count);
                }
                (revidx, cs.0.clone())
            }
        })
        .map_err({
            let logger = logger.clone();
            move |err| {
                let msg = format!("failed to blobimport: {}", err);
                error!(logger, "{}", msg);

                let mut err = err.deref() as &dyn StdError;
                while let Some(cause) = failure_ext::cause(err) {
                    info!(logger, "cause: {}", cause);
                    err = cause;
                }
                info!(logger, "root cause: {:?}", err);

                Error::msg(msg)
            }
        });

        // Blobimport does not see scratch bookmarks in Mercurial, so we use
        // PublishingOrPullDefault here, which is the non-scratch set in Mononoke.
        let mononoke_bookmarks = blobrepo
            .get_bonsai_publishing_bookmarks_maybe_stale(ctx.clone())
            .map(|(bookmark, changeset_id)| (bookmark.into_name(), changeset_id));

        stale_bookmarks
            .join(mononoke_bookmarks.collect())
            .and_then({
                cloned!(ctx, blobrepo, logger);
                move |(stale_bookmarks, mononoke_bookmarks)| {
                    upload_changesets
                        .chunks(chunk_size)
                        .and_then({
                            cloned!(
                                ctx,
                                globalrevs_store,
                                synced_commit_mapping,
                                blobrepo,
                                logger
                            );
                            move |chunk| {
                                let max_rev = chunk.iter().map(|(rev, _)| rev).max().cloned();
                                let synced_commit_mapping_work =
                                    if let Some(small_repo_id) = small_repo_id {
                                        let entries = chunk
                                            .iter()
                                            .map(|(_, cs)| SyncedCommitMappingEntry {
                                                large_repo_id: repo_id,
                                                large_bcs_id: cs.get_changeset_id(),
                                                small_repo_id,
                                                small_bcs_id: cs.get_changeset_id(),
                                            })
                                            .collect();
                                        synced_commit_mapping
                                            .add_bulk(ctx.clone(), entries)
                                            .map(|_| ())
                                            .left_future()
                                    } else {
                                        future::ok(()).right_future()
                                    };

                                let changesets: Vec<_> =
                                    chunk.into_iter().map(|(_, cs)| cs).collect();

                                let globalrevs_work = if has_globalrev {
                                    bulk_import_globalrevs(
                                        ctx.clone(),
                                        repo_id,
                                        globalrevs_store.clone(),
                                        changesets.iter(),
                                    )
                                    .left_future()
                                } else {
                                    future::ok(()).right_future()
                                };

                                let git_mapping_work = {
                                    cloned!(changesets, ctx);
                                    let git_mapping_store = blobrepo.bonsai_git_mapping().clone();
                                    async move {
                                        if populate_git_mapping {
                                            git_mapping_store
                                                .bulk_import_from_bonsai(ctx, &changesets)
                                                .await
                                        } else {
                                            Ok(())
                                        }
                                    }
                                    .boxed()
                                    .compat()
                                };

                                if !derived_data_types.is_empty() {
                                    info!(logger, "Deriving data for: {:?}", derived_data_types);
                                }

                                let derivation_work = derive_data_for_csids(
                                    ctx.clone(),
                                    blobrepo.clone(),
                                    changesets.iter().map(|cs| cs.get_changeset_id()).collect(),
                                    &derived_data_types[..],
                                );

                                let derivation_work =
                                    async move { derivation_work?.await }.boxed().compat();

                                globalrevs_work
                                    .join(synced_commit_mapping_work)
                                    .join(derivation_work)
                                    .join(git_mapping_work)
                                    .map(move |_| max_rev)
                            }
                        })
                        .fold(None, |mut acc, rev| {
                            if let Some(rev) = rev {
                                acc = Some(::std::cmp::max(acc.unwrap_or(RevIdx::zero()), rev));
                            }
                            let res: Result<_, Error> = Ok(acc);
                            res
                        })
                        .map(move |max_rev| (max_rev, stale_bookmarks, mononoke_bookmarks))
                }
            })
            .and_then(move |(max_rev, stale_bookmarks, mononoke_bookmarks)| {
                info!(
                    logger,
                    "finished uploading changesets, globalrevs and deriving data"
                );
                let f = match bookmark_import_policy {
                    BookmarkImportPolicy::Ignore => {
                        info!(
                            logger,
                            "since --no-bookmark was provided, bookmarks won't be imported"
                        );
                        future::ok(()).boxify()
                    }
                    BookmarkImportPolicy::Prefix(prefix) => bookmark::upload_bookmarks(
                        ctx,
                        &logger,
                        revlogrepo,
                        blobrepo,
                        stale_bookmarks,
                        mononoke_bookmarks,
                        bookmark::get_bookmark_prefixer(prefix),
                    ),
                };
                f.map(move |()| max_rev)
            })
            .boxify()
    }
}
