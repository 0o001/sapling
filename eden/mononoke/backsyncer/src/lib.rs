/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

/// Backsyncer
///
/// Library to sync commits from source repo to target repo by following bookmark update log
/// and doing commit rewrites. The main motivation for backsyncer is to keep "small repo" up to
/// date with "large repo" in a setup where all writes to small repo are redirected to large repo
/// in a push redirector.
/// More details can be found here - https://fb.quip.com/tZ4yAaA3S4Mc
///
/// Target repo tails source repo's bookmark update log and backsync bookmark updates one by one.
/// The latest backsynced log id is stored in mutable_counters table. Backsync consists of the
/// following phases:
///
/// 1) Given an entry from bookmark update log of a target repo,
///    find commits to backsync from source repo into a target repo.
/// 2) Rewrite these commits and create rewritten commits in target repo
/// 3) In the same transaction try to update a bookmark in the source repo AND latest backsynced
///    log id.
use anyhow::{bail, format_err, Error};
use blobrepo::BlobRepo;
use blobrepo_factory::ReadOnlyStorage;
use blobstore_factory::make_metadata_sql_factory;
use bookmarks::{
    BookmarkTransactionError, BookmarkUpdateLogEntry, BookmarkUpdateReason, Bookmarks, Freshness,
};
use cloned::cloned;
use context::CoreContext;
use cross_repo_sync::{CommitSyncOutcome, CommitSyncer};
use futures::{
    compat::Future01CompatExt,
    future::{FutureExt, TryFutureExt},
};
use futures_old::future::Future;
use futures_old::stream::Stream as OldStream;
use metaconfig_types::MetadataDatabaseConfig;
use mononoke_types::{ChangesetId, RepositoryId};
use mutable_counters::{MutableCounters, SqlMutableCounters};
use slog::debug;
use sql::Transaction;
use sql_construct::SqlConstruct;
use sql_ext::facebook::MysqlOptions;
use sql_ext::{SqlConnections, TransactionResult};
use std::{convert::TryFrom, sync::Arc, time::Instant};
use synced_commit_mapping::SyncedCommitMapping;
use thiserror::Error;

#[cfg(test)]
mod tests;

#[derive(Debug, Error)]
pub enum BacksyncError {
    #[error("BacksyncError::LogEntryNotFound: {latest_log_id} not found")]
    LogEntryNotFound { latest_log_id: u64 },
    #[error("BacksyncError::Other")]
    Other(#[from] Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacksyncLimit {
    NoLimit,
    Limit(u64),
}

pub async fn backsync_latest<M>(
    ctx: CoreContext,
    commit_syncer: CommitSyncer<M>,
    target_repo_dbs: TargetRepoDbs,
    limit: BacksyncLimit,
) -> Result<(), Error>
where
    M: SyncedCommitMapping + Clone + 'static,
{
    let TargetRepoDbs { ref counters, .. } = target_repo_dbs;
    let target_repo_id = commit_syncer.get_target_repo().get_repoid();
    let source_repo_id = commit_syncer.get_source_repo().get_repoid();
    let counter_name = format_counter(&source_repo_id);

    let counter = counters
        .get_counter(ctx.clone(), target_repo_id, &counter_name)
        .compat()
        .await?
        .unwrap_or(0);

    debug!(ctx.logger(), "fetched counter {}", counter);

    let log_entries_limit = match limit {
        BacksyncLimit::Limit(limit) => limit,
        BacksyncLimit::NoLimit => {
            // Set limit extremely high to read all new values
            u64::max_value()
        }
    };
    let next_entries = commit_syncer
        .get_source_repo()
        .read_next_bookmark_log_entries(
            ctx.clone(),
            counter as u64,
            log_entries_limit,
            Freshness::MostRecent,
        )
        .collect()
        .compat()
        .await?;

    if next_entries.is_empty() {
        debug!(ctx.logger(), "nothing to sync");
        Ok(())
    } else {
        sync_entries(
            ctx,
            &commit_syncer,
            target_repo_dbs,
            next_entries,
            counter as i64,
        )
        .await
    }
}

async fn sync_entries<M>(
    ctx: CoreContext,
    commit_syncer: &CommitSyncer<M>,
    target_repo_dbs: TargetRepoDbs,
    entries: Vec<BookmarkUpdateLogEntry>,
    mut counter: i64,
) -> Result<(), Error>
where
    M: SyncedCommitMapping + Clone + 'static,
{
    for entry in entries {
        let entry_id = entry.id;
        if counter >= entry_id {
            continue;
        }
        debug!(ctx.logger(), "backsyncing {} ...", entry_id);

        let mut scuba_sample = ctx.scuba().clone();
        scuba_sample.add("backsyncer_bookmark_log_entry_id", entry.id);

        let start_instant = Instant::now();

        if let Some(to_cs_id) = entry.to_changeset_id {
            commit_syncer.sync_commit(&ctx, to_cs_id).await?;
        }

        let new_counter = entry.id;
        let success = backsync_bookmark(
            ctx.clone(),
            commit_syncer,
            target_repo_dbs.clone(),
            Some(counter),
            entry,
        )
        .await?;

        scuba_sample.add(
            "backsync_duration_ms",
            u64::try_from(start_instant.elapsed().as_millis()).unwrap_or(u64::max_value()),
        );
        scuba_sample.add("backsync_previously_done", !success);
        scuba_sample.log();

        if success {
            counter = new_counter;
        } else {
            debug!(
                ctx.logger(),
                "failed to backsync {}, most likely another process already synced it ", entry_id
            );
            // Transaction failed, it could be because another process already backsynced it
            // Verify that counter was moved and continue if that's the case

            let source_repo_id = commit_syncer.get_source_repo().get_repoid();
            let target_repo_id = commit_syncer.get_target_repo().get_repoid();
            let counter_name = format_counter(&source_repo_id);
            let new_counter = target_repo_dbs
                .counters
                .get_counter(ctx.clone(), target_repo_id, &counter_name)
                .compat()
                .await?
                .unwrap_or(0);
            if new_counter <= counter {
                return Err(format_err!(
                    "backsync transaction failed, but the counter didn't move forward. Was {}, became {}",
                    counter, new_counter,
                ));
            } else {
                debug!(
                    ctx.logger(),
                    "verified that another process has already synced {}", entry_id
                );
                counter = new_counter;
            }
        }
    }
    Ok(())
}

async fn backsync_bookmark<M>(
    ctx: CoreContext,
    commit_syncer: &CommitSyncer<M>,
    target_repo_dbs: TargetRepoDbs,
    prev_counter: Option<i64>,
    log_entry: BookmarkUpdateLogEntry,
) -> Result<bool, Error>
where
    M: SyncedCommitMapping + Clone + 'static,
{
    let target_repo_id = commit_syncer.get_target_repo().get_repoid();
    let source_repo_id = commit_syncer.get_source_repo().get_repoid();
    let TargetRepoDbs {
        connections,
        bookmarks,
        ..
    } = target_repo_dbs;

    debug!(ctx.logger(), "preparing to backsync {:?}", log_entry);

    let new_counter = log_entry.id;
    let bookmark = commit_syncer.get_bookmark_renamer()(&log_entry.bookmark_name);
    debug!(ctx.logger(), "bookmark was renamed into {:?}", bookmark);
    let from_cs_id = log_entry.from_changeset_id;
    let to_cs_id = log_entry.to_changeset_id;

    let get_commit_sync_outcome = |maybe_cs_id: Option<ChangesetId>| {
        cloned!(ctx);
        async move {
            match maybe_cs_id {
                Some(cs_id) => {
                    let maybe_outcome = commit_syncer
                        .get_commit_sync_outcome(ctx.clone(), cs_id)
                        .await?;
                    match maybe_outcome {
                        Some(outcome) => Ok(Some((outcome, cs_id))),
                        None => Err(format_err!("{} hasn't been backsynced yet", cs_id)),
                    }
                }
                None => Ok(None),
            }
        }
    };

    let get_remapped_cs_id =
        move |maybe_outcome: Option<(CommitSyncOutcome, ChangesetId)>| match maybe_outcome {
            Some((outcome, cs_id)) => {
                use CommitSyncOutcome::*;
                match outcome {
                    NotSyncCandidate => Err(format_err!(
                        "invalid bookmark move: {:?} should not be synced to target repo",
                        cs_id
                    )),
                    RewrittenAs(cs_id) | EquivalentWorkingCopyAncestor(cs_id) => Ok(Some(cs_id)),
                    Preserved => Ok(Some(cs_id)),
                }
            }
            None => Ok(None),
        };

    let txn_hook = Arc::new({
        move |ctx: CoreContext, txn: Transaction| {
            async move {
                let txn = SqlMutableCounters::set_counter_on_txn(
                    ctx.clone(),
                    target_repo_id,
                    &format_counter(&source_repo_id),
                    new_counter,
                    prev_counter,
                    txn,
                )
                .compat()
                .await?;

                match txn {
                    TransactionResult::Succeeded(txn) => Ok(txn),
                    TransactionResult::Failed => Err(BookmarkTransactionError::LogicError),
                }
            }
            .boxed()
        }
    });

    if let Some(bookmark) = bookmark {
        // Fetch sync outcome before transaction to keep transaction as short as possible
        let from_sync_outcome = get_commit_sync_outcome(from_cs_id).await?;
        let to_sync_outcome = get_commit_sync_outcome(to_cs_id).await?;
        debug!(
            ctx.logger(),
            "commit sync outcomes: from_cs: {:?}, to_cs: {:?}", from_sync_outcome, to_sync_outcome
        );

        let from_cs_id = get_remapped_cs_id(from_sync_outcome)?;
        let to_cs_id = get_remapped_cs_id(to_sync_outcome)?;

        if from_cs_id != to_cs_id {
            let mut bookmark_txn = bookmarks.create_transaction(ctx.clone(), target_repo_id);
            debug!(
                ctx.logger(),
                "syncing bookmark {} to {:?}", bookmark, to_cs_id
            );
            let reason = BookmarkUpdateReason::Backsyncer {
                bundle_replay_data: log_entry.reason.get_bundle_replay_data().cloned(),
            };

            match (from_cs_id, to_cs_id) {
                (Some(from), Some(to)) => {
                    debug!(
                        ctx.logger(),
                        "updating bookmark {:?} from {:?} to {:?}", bookmark, from, to
                    );
                    bookmark_txn.update(&bookmark, to, from, reason)?;
                }
                (Some(from), None) => {
                    debug!(
                        ctx.logger(),
                        "deleting bookmark {:?} with original position {:?}", bookmark, from
                    );
                    bookmark_txn.delete(&bookmark, from, reason)?;
                }
                (None, Some(to)) => {
                    debug!(
                        ctx.logger(),
                        "creating bookmark {:?} to point to {:?}", bookmark, to
                    );
                    bookmark_txn.create(&bookmark, to, reason)?;
                }
                (None, None) => {
                    bail!("unexpected bookmark move");
                }
            };

            return bookmark_txn.commit_with_hook(txn_hook).await;
        } else {
            debug!(
                ctx.logger(),
                "from_cs_id and to_cs_id are the same: {:?}. No sync happening for {:?}",
                from_cs_id,
                bookmark
            );
        }
    } else {
        debug!(ctx.logger(), "Renamed bookmark is None. No sync happening.");
    }

    let updated = SqlMutableCounters::from_sql_connections(connections)
        .set_counter(
            ctx.clone(),
            target_repo_id,
            &format_counter(&source_repo_id),
            new_counter,
            prev_counter,
        )
        .compat()
        .await?;

    Ok(updated)
}

// TODO(stash): T56228235 - consider removing SqlMutableCounters and SqlBookmarks and use static
// methods instead
#[derive(Clone)]
pub struct TargetRepoDbs {
    pub connections: SqlConnections,
    pub bookmarks: Arc<dyn Bookmarks>,
    pub counters: SqlMutableCounters,
}

pub fn open_backsyncer_dbs_compat(
    ctx: CoreContext,
    blobrepo: BlobRepo,
    db_config: MetadataDatabaseConfig,
    mysql_options: MysqlOptions,
    readonly_storage: ReadOnlyStorage,
) -> impl Future<Item = TargetRepoDbs, Error = Error> {
    open_backsyncer_dbs(ctx, blobrepo, db_config, mysql_options, readonly_storage)
        .boxed()
        .compat()
}

pub async fn open_backsyncer_dbs(
    ctx: CoreContext,
    blobrepo: BlobRepo,
    db_config: MetadataDatabaseConfig,
    mysql_options: MysqlOptions,
    readonly_storage: ReadOnlyStorage,
) -> Result<TargetRepoDbs, Error> {
    let sql_factory = make_metadata_sql_factory(
        ctx.fb,
        db_config,
        mysql_options,
        readonly_storage,
        ctx.logger().clone(),
    )
    .compat()
    .await?;

    let connections = sql_factory
        .make_primary_connections("bookmark_mutable_counters".to_string())
        .compat()
        .await?;

    let counters = SqlMutableCounters::from_sql_connections(connections.clone());

    Ok(TargetRepoDbs {
        connections,
        bookmarks: blobrepo.get_bookmarks_object(),
        counters,
    })
}

pub fn format_counter(source_repo_id: &RepositoryId) -> String {
    format!("backsync_from_{}", source_repo_id.id())
}
