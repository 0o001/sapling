// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use blobstore::Blobstore;
use blobstore_sync_queue::{BlobstoreSyncQueue, BlobstoreSyncQueueEntry};
use chrono::Duration as ChronoDuration;
use context::CoreContext;
use failure::{err_msg, prelude::*};
use futures::{
    future::{join_all, loop_fn, Loop},
    prelude::*,
};
use futures_ext::FutureExt;
use itertools::Itertools;
use metaconfig_types::BlobstoreId;
use mononoke_types::{BlobstoreBytes, DateTime, RepositoryId};
use rate_limiter::RateLimiter;
use slog::Logger;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

lazy_static! {
    /// Minimal age of entry to consider if it has to be healed
    static ref ENTRY_HEALING_MIN_AGE: ChronoDuration = ChronoDuration::minutes(2);
}

pub struct RepoHealer {
    logger: Logger,
    blobstore_sync_queue_limit: usize,
    repo_id: RepositoryId,
    rate_limiter: RateLimiter,
    sync_queue: Arc<BlobstoreSyncQueue>,
    blobstores: Arc<HashMap<BlobstoreId, Arc<Blobstore>>>,
}

impl RepoHealer {
    pub fn new(
        logger: Logger,
        blobstore_sync_queue_limit: usize,
        repo_id: RepositoryId,
        rate_limiter: RateLimiter,
        sync_queue: Arc<BlobstoreSyncQueue>,
        blobstores: Arc<HashMap<BlobstoreId, Arc<Blobstore>>>,
    ) -> Self {
        Self {
            logger,
            blobstore_sync_queue_limit,
            repo_id,
            rate_limiter,
            sync_queue,
            blobstores,
        }
    }

    pub fn heal(&self, ctx: CoreContext) -> impl Future<Item = (), Error = Error> {
        cloned!(
            self.logger,
            self.blobstore_sync_queue_limit,
            self.repo_id,
            self.rate_limiter,
            self.sync_queue,
            self.blobstores
        );

        let now = DateTime::now().into_chrono();
        let healing_deadline = DateTime::new(now - *ENTRY_HEALING_MIN_AGE);

        sync_queue
            .iter(
                ctx.clone(),
                repo_id,
                healing_deadline.clone(),
                blobstore_sync_queue_limit,
            )
            .and_then(move |queue_entries| {
                cloned!(rate_limiter);

                let healing_futures: Vec<_> = queue_entries
                    .into_iter()
                    .group_by(|entry| entry.blobstore_key.clone())
                    .into_iter()
                    .filter_map(|(key, entries)| {
                        cloned!(ctx, sync_queue, blobstores, healing_deadline);
                        heal_blob(
                            ctx,
                            repo_id,
                            sync_queue,
                            blobstores,
                            healing_deadline,
                            key,
                            entries.collect(),
                        )
                    })
                    .map(move |healing_future| rate_limiter.execute(healing_future))
                    .collect();

                info!(
                    logger,
                    "Found {} blobs to be healed... Doing it",
                    healing_futures.len()
                );

                join_all(healing_futures)
            })
            .map(|_| ())
    }
}

fn heal_blob(
    ctx: CoreContext,
    repo_id: RepositoryId,
    sync_queue: Arc<BlobstoreSyncQueue>,
    blobstores: Arc<HashMap<BlobstoreId, Arc<Blobstore>>>,
    healing_deadline: DateTime,
    key: String,
    entries: Vec<BlobstoreSyncQueueEntry>,
) -> Option<impl Future<Item = (), Error = Error>> {
    let seen_blobstores: HashSet<_> = entries
        .iter()
        .filter_map(|entry| {
            let id = entry.blobstore_id.clone();
            if blobstores.contains_key(&id) {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    let missing_blobstores: HashSet<_> = blobstores
        .iter()
        .filter_map(|(key, _)| {
            if seen_blobstores.contains(key) {
                None
            } else {
                Some(key.clone())
            }
        })
        .collect();

    if missing_blobstores.is_empty() {
        // All blobstores have been synchronized
        return Some(cleanup_after_healing(ctx, sync_queue, entries).left_future());
    }

    if !entries
        .iter()
        .any(|entry| entry.timestamp < healing_deadline)
    {
        // The oldes entry is not old enough to be eligible for healing
        return None;
    }

    let heal_future = fetch_blob(
        ctx.clone(),
        blobstores.clone(),
        key.clone(),
        seen_blobstores,
    )
    .and_then(move |blob| {
        let heal_blobstores: Vec<_> = missing_blobstores
            .into_iter()
            .map(|bid| {
                let blobstore = blobstores
                    .get(&bid)
                    .expect("missing_blobstores contains only existing blobstores");
                blobstore
                    .put(ctx.clone(), key.clone(), blob.clone())
                    .then(move |result| Ok((bid, result.is_ok())))
            })
            .collect();

        join_all(heal_blobstores).and_then(move |heal_results| {
            if heal_results.iter().all(|(_, result)| *result) {
                cleanup_after_healing(ctx, sync_queue, entries).left_future()
            } else {
                let healed_blobstores =
                    heal_results
                        .into_iter()
                        .filter_map(|(id, result)| if result { Some(id) } else { None });
                report_partial_heal(ctx, repo_id, sync_queue, key, healed_blobstores).right_future()
            }
        })
    });

    Some(heal_future.right_future())
}

fn fetch_blob(
    ctx: CoreContext,
    blobstores: Arc<HashMap<BlobstoreId, Arc<Blobstore>>>,
    key: String,
    seen_blobstores: HashSet<BlobstoreId>,
) -> impl Future<Item = BlobstoreBytes, Error = Error> {
    let blobstores_to_fetch: Vec<_> = seen_blobstores.iter().cloned().collect();
    let err_context = format!(
        "While fetching blob '{}', seen in blobstores: {:?}",
        key, seen_blobstores
    );

    loop_fn(blobstores_to_fetch, move |mut blobstores_to_fetch| {
        let bid = match blobstores_to_fetch.pop() {
            None => {
                return Err(err_msg("None of the blobstores to fetch responded"))
                    .into_future()
                    .left_future();
            }
            Some(bid) => bid,
        };

        let blobstore = blobstores
            .get(&bid)
            .expect("blobstores_to_fetch contains only existing blobstores");

        blobstore
            .get(ctx.clone(), key.clone())
            .then(move |result| match result {
                Err(_) => return Ok(Loop::Continue(blobstores_to_fetch)),
                Ok(None) => {
                    return Err(format_err!(
                        "Blobstore {:?} retruned None even though it should contain data",
                        bid
                    ));
                }
                Ok(Some(blob)) => Ok(Loop::Break(blob)),
            })
            .right_future()
    })
    .chain_err(err_context)
    .from_err()
}

fn cleanup_after_healing(
    ctx: CoreContext,
    sync_queue: Arc<BlobstoreSyncQueue>,
    entries: Vec<BlobstoreSyncQueueEntry>,
) -> impl Future<Item = (), Error = Error> {
    sync_queue.del(ctx, entries)
}

fn report_partial_heal(
    ctx: CoreContext,
    repo_id: RepositoryId,
    sync_queue: Arc<BlobstoreSyncQueue>,
    blobstore_key: String,
    healed_blobstores: impl IntoIterator<Item = BlobstoreId>,
) -> impl Future<Item = (), Error = Error> {
    let timestamp = DateTime::now();

    join_all(healed_blobstores.into_iter().map({
        move |blobstore_id| {
            cloned!(ctx, repo_id, blobstore_key, timestamp);
            sync_queue.add(
                ctx,
                BlobstoreSyncQueueEntry {
                    repo_id,
                    blobstore_key,
                    blobstore_id,
                    timestamp,
                    id: None,
                },
            )
        }
    }))
    .map(|_| ())
}
