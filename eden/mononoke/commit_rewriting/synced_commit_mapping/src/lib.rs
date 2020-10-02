/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use std::sync::Arc;

use sql::{Connection, Transaction};
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::SqlConnections;

use anyhow::Error;
use cloned::cloned;
use context::CoreContext;
use futures_ext::{BoxFuture, FutureExt};
use futures_old::{future, Future};
use metaconfig_types::CommitSyncConfigVersion;
use mononoke_types::{ChangesetId, RepositoryId};
use sql::queries;
use stats::prelude::*;
use thiserror::Error;

#[derive(Debug, Eq, Error, PartialEq)]
pub enum ErrorKind {
    #[error(
        "tried to insert inconsistent small bcs id {expected_bcs_id:?} version {expected_config_version:?}, while db has {actual_bcs_id:?} version {actual_config_version:?}"
    )]
    InconsistentWorkingCopyEntry {
        expected_bcs_id: Option<ChangesetId>,
        expected_config_version: Option<CommitSyncConfigVersion>,
        actual_bcs_id: Option<ChangesetId>,
        actual_config_version: Option<CommitSyncConfigVersion>,
    },
}

// TODO(simonfar): Once we've proven the concept, we want to cache these
define_stats! {
    prefix = "mononoke.synced_commit_mapping";
    gets: timeseries(Rate, Sum),
    gets_master: timeseries(Rate, Sum),
    adds: timeseries(Rate, Sum),
    add_many_in_txn: timeseries(Rate, Sum),
    add_bulks: timeseries(Rate, Sum),
    insert_working_copy_eqivalence: timeseries(Rate, Sum),
    get_equivalent_working_copy: timeseries(Rate, Sum),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SyncedCommitMappingEntry {
    pub large_repo_id: RepositoryId,
    pub large_bcs_id: ChangesetId,
    pub small_repo_id: RepositoryId,
    pub small_bcs_id: ChangesetId,
    pub version_name: Option<CommitSyncConfigVersion>,
}

impl SyncedCommitMappingEntry {
    pub fn new(
        large_repo_id: RepositoryId,
        large_bcs_id: ChangesetId,
        small_repo_id: RepositoryId,
        small_bcs_id: ChangesetId,
        version_name: Option<CommitSyncConfigVersion>,
    ) -> Self {
        Self {
            large_repo_id,
            large_bcs_id,
            small_repo_id,
            small_bcs_id,
            version_name,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EquivalentWorkingCopyEntry {
    pub large_repo_id: RepositoryId,
    pub large_bcs_id: ChangesetId,
    pub small_repo_id: RepositoryId,
    pub small_bcs_id: Option<ChangesetId>,
    pub version_name: Option<CommitSyncConfigVersion>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkingCopyEquivalence {
    /// There's no matching working copy. It can happen if a pre-big-merge commit from one small
    /// repo is mapped into another small repo
    NoWorkingCopy(Option<CommitSyncConfigVersion>),
    /// ChangesetId of matching working copy and CommitSyncConfigVersion that was used for mapping
    WorkingCopy(ChangesetId, Option<CommitSyncConfigVersion>),
}

pub trait SyncedCommitMapping: Send + Sync {
    /// Given the full large, small mapping, store it in the DB.
    /// Future resolves to true if the mapping was saved, false otherwise
    fn add(&self, ctx: CoreContext, entry: SyncedCommitMappingEntry) -> BoxFuture<bool, Error>;

    /// Bulk insert a set of large, small mappings
    /// This is meant for blobimport and similar
    fn add_bulk(
        &self,
        ctx: CoreContext,
        entries: Vec<SyncedCommitMappingEntry>,
    ) -> BoxFuture<u64, Error>;

    /// Find all the mapping entries for a given source commit and target repo
    fn get(
        &self,
        ctx: CoreContext,
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Vec<(ChangesetId, Option<CommitSyncConfigVersion>)>, Error>;

    /// Inserts equivalent working copy of a large bcs id. It's similar to mapping entry,
    /// however there are a few differences:
    /// 1) For (large repo, small repo) pair, many large commits can map to the same small commit
    /// 2) Small commit can be null
    ///
    /// If there's a mapping between small and large commits, then equivalent working copy is
    /// the same as the same as the mapping.
    fn insert_equivalent_working_copy(
        &self,
        ctx: CoreContext,
        entry: EquivalentWorkingCopyEntry,
    ) -> BoxFuture<bool, Error>;

    /// Finds equivalent working copy
    fn get_equivalent_working_copy(
        &self,
        ctx: CoreContext,
        source_repo_id: RepositoryId,
        source_bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Option<WorkingCopyEquivalence>, Error>;
}

impl SyncedCommitMapping for Arc<dyn SyncedCommitMapping> {
    fn add(&self, ctx: CoreContext, entry: SyncedCommitMappingEntry) -> BoxFuture<bool, Error> {
        (**self).add(ctx, entry)
    }

    fn add_bulk(
        &self,
        ctx: CoreContext,
        entries: Vec<SyncedCommitMappingEntry>,
    ) -> BoxFuture<u64, Error> {
        (**self).add_bulk(ctx, entries)
    }

    fn get(
        &self,
        ctx: CoreContext,
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Vec<(ChangesetId, Option<CommitSyncConfigVersion>)>, Error> {
        (**self).get(ctx, source_repo_id, bcs_id, target_repo_id)
    }

    fn insert_equivalent_working_copy(
        &self,
        ctx: CoreContext,
        entry: EquivalentWorkingCopyEntry,
    ) -> BoxFuture<bool, Error> {
        (**self).insert_equivalent_working_copy(ctx, entry)
    }

    fn get_equivalent_working_copy(
        &self,
        ctx: CoreContext,
        source_repo_id: RepositoryId,
        source_bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Option<WorkingCopyEquivalence>, Error> {
        (**self).get_equivalent_working_copy(ctx, source_repo_id, source_bcs_id, target_repo_id)
    }
}

#[derive(Clone)]
pub struct SqlSyncedCommitMapping {
    write_connection: Connection,
    read_connection: Connection,
    read_master_connection: Connection,
}

queries! {
    write InsertMapping(values: (
        large_repo_id: RepositoryId,
        large_bcs_id: ChangesetId,
        small_repo_id: RepositoryId,
        small_bcs_id: ChangesetId,
        sync_map_version_name: Option<String>,
    )) {
        insert_or_ignore,
        "{insert_or_ignore} INTO synced_commit_mapping (large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name) VALUES {values}"
    }

    read SelectMapping(
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> (RepositoryId, ChangesetId, RepositoryId, ChangesetId, Option<String>) {
        "SELECT large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name
         FROM synced_commit_mapping
         WHERE (large_repo_id = {source_repo_id} AND large_bcs_id = {bcs_id} AND small_repo_id = {target_repo_id}) OR
         (small_repo_id = {source_repo_id} AND small_bcs_id = {bcs_id} AND large_repo_id = {target_repo_id})"
    }

    write InsertWorkingCopyEquivalence(values: (
        large_repo_id: RepositoryId,
        large_bcs_id: ChangesetId,
        small_repo_id: RepositoryId,
        small_bcs_id: Option<ChangesetId>,
        sync_map_version_name: Option<String>,
    )) {
        insert_or_ignore,
        "{insert_or_ignore}
        INTO synced_working_copy_equivalence
        (large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name)
        VALUES {values}"
    }

    read SelectWorkingCopyEquivalence(
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> (RepositoryId, ChangesetId, RepositoryId, Option<ChangesetId>, Option<String>) {
        "SELECT large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name
         FROM synced_working_copy_equivalence
         WHERE (large_repo_id = {source_repo_id} AND small_repo_id = {target_repo_id} AND large_bcs_id = {bcs_id})
         OR (large_repo_id = {target_repo_id} AND small_repo_id = {source_repo_id} AND small_bcs_id = {bcs_id})
         ORDER BY mapping_id ASC
         LIMIT 1
         "
    }
}

impl SqlConstruct for SqlSyncedCommitMapping {
    const LABEL: &'static str = "synced_commit_mapping";

    const CREATION_QUERY: &'static str =
        include_str!("../schemas/sqlite-synced-commit-mapping.sql");

    fn from_sql_connections(connections: SqlConnections) -> Self {
        Self {
            write_connection: connections.write_connection,
            read_connection: connections.read_connection,
            read_master_connection: connections.read_master_connection,
        }
    }
}

impl SqlConstructFromMetadataDatabaseConfig for SqlSyncedCommitMapping {}

impl SqlSyncedCommitMapping {
    fn add_many(
        &self,
        entries: Vec<SyncedCommitMappingEntry>,
    ) -> impl Future<Item = u64, Error = Error> {
        self.write_connection
            .start_transaction()
            .and_then(move |txn| {
                add_many_in_txn(txn, entries)
                    .and_then(|(txn, affected_rows)| txn.commit().map(move |()| affected_rows))
            })
    }
}

impl SyncedCommitMapping for SqlSyncedCommitMapping {
    fn add(&self, _ctx: CoreContext, entry: SyncedCommitMappingEntry) -> BoxFuture<bool, Error> {
        STATS::adds.add_value(1);

        self.add_many(vec![entry]).map(|count| count == 1).boxify()
    }

    fn add_bulk(
        &self,
        _ctx: CoreContext,
        entries: Vec<SyncedCommitMappingEntry>,
    ) -> BoxFuture<u64, Error> {
        STATS::add_bulks.add_value(1);

        self.add_many(entries).boxify()
    }

    fn get(
        &self,
        _ctx: CoreContext,
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Vec<(ChangesetId, Option<CommitSyncConfigVersion>)>, Error> {
        STATS::gets.add_value(1);

        SelectMapping::query(
            &self.read_connection,
            &source_repo_id,
            &bcs_id,
            &target_repo_id,
        )
        .and_then({
            cloned!(self.read_master_connection);
            move |rows| {
                if rows.is_empty() {
                    STATS::gets_master.add_value(1);
                    SelectMapping::query(
                        &read_master_connection,
                        &source_repo_id,
                        &bcs_id,
                        &target_repo_id,
                    )
                    .left_future()
                } else {
                    future::ok(rows).right_future()
                }
            }
        })
        .map(move |rows| {
            let v: Vec<_> = rows
                .iter()
                .map(|row| {
                    let (
                        large_repo_id,
                        large_bcs_id,
                        _small_repo_id,
                        small_bcs_id,
                        ref version_name,
                    ) = row;
                    let maybe_version_name: Option<CommitSyncConfigVersion> = match version_name {
                        Some(version_name) => {
                            Some(CommitSyncConfigVersion(version_name.to_owned()))
                        }
                        None => None,
                    };
                    if target_repo_id == *large_repo_id {
                        (*large_bcs_id, maybe_version_name)
                    } else {
                        (*small_bcs_id, maybe_version_name)
                    }
                })
                .collect();

            v
        })
        .boxify()
    }

    fn insert_equivalent_working_copy(
        &self,
        ctx: CoreContext,
        entry: EquivalentWorkingCopyEntry,
    ) -> BoxFuture<bool, Error> {
        STATS::insert_working_copy_eqivalence.add_value(1);

        let EquivalentWorkingCopyEntry {
            large_repo_id,
            large_bcs_id,
            small_repo_id,
            small_bcs_id,
            version_name,
        } = entry;

        let version_name_clone = version_name.clone();
        let this = self.clone();
        InsertWorkingCopyEquivalence::query(
            &self.write_connection,
            &[(
                &large_repo_id,
                &large_bcs_id,
                &small_repo_id,
                &small_bcs_id,
                &version_name.map(|vn| vn.0),
            )],
        )
        .and_then(move |result| {
            if result.affected_rows() == 1 {
                future::ok(true).left_future()
            } else {
                // Check that db stores consistent entry
                this.get_equivalent_working_copy(
                    ctx.clone(),
                    large_repo_id,
                    large_bcs_id,
                    small_repo_id,
                )
                .and_then(move |maybe_equivalent_wc| {
                    if let Some(equivalent_wc) = maybe_equivalent_wc {
                        use WorkingCopyEquivalence::*;
                        let (expected_bcs_id, expected_version) = match equivalent_wc {
                            WorkingCopy(wc, mapping) => (Some(wc), mapping),
                            NoWorkingCopy(mapping) => (None, mapping),
                        };
                        if (expected_bcs_id != small_bcs_id)
                            || (expected_version != version_name_clone)
                        {
                            let err = ErrorKind::InconsistentWorkingCopyEntry {
                                expected_bcs_id,
                                expected_config_version: expected_version,
                                actual_bcs_id: small_bcs_id,
                                actual_config_version: version_name_clone,
                            };
                            return Err(err.into());
                        }
                    }
                    Ok(false)
                })
                .right_future()
            }
        })
        .boxify()
    }

    fn get_equivalent_working_copy(
        &self,
        _ctx: CoreContext,
        source_repo_id: RepositoryId,
        source_bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> BoxFuture<Option<WorkingCopyEquivalence>, Error> {
        STATS::get_equivalent_working_copy.add_value(1);

        cloned!(self.read_master_connection);
        SelectWorkingCopyEquivalence::query(
            &self.read_connection,
            &source_repo_id,
            &source_bcs_id,
            &target_repo_id,
        )
        .and_then(move |rows| {
            if rows.len() >= 1 {
                future::ok(rows.get(0).cloned()).left_future()
            } else {
                SelectWorkingCopyEquivalence::query(
                    &read_master_connection,
                    &source_repo_id,
                    &source_bcs_id,
                    &target_repo_id,
                )
                .map(|rows| rows.get(0).cloned())
                .right_future()
            }
        })
        .map(move |maybe_row| {
            match maybe_row {
                Some(row) => {
                    let (
                        large_repo_id,
                        large_bcs_id,
                        _small_repo_id,
                        maybe_small_bcs_id,
                        maybe_mapping,
                    ) = row;

                    if target_repo_id == large_repo_id {
                        Some(WorkingCopyEquivalence::WorkingCopy(
                            large_bcs_id,
                            maybe_mapping.map(CommitSyncConfigVersion),
                        ))
                    } else {
                        match maybe_small_bcs_id {
                            Some(small_bcs_id) => Some(WorkingCopyEquivalence::WorkingCopy(
                                small_bcs_id,
                                maybe_mapping.map(CommitSyncConfigVersion),
                            )),
                            None => Some(WorkingCopyEquivalence::NoWorkingCopy(
                                maybe_mapping.map(CommitSyncConfigVersion),
                            )),
                        }
                    }
                }
                None => None,
            }
        })
        .boxify()
    }
}

pub fn add_many_in_txn(
    txn: Transaction,
    entries: Vec<SyncedCommitMappingEntry>,
) -> impl Future<Item = (Transaction, u64), Error = Error> {
    STATS::add_many_in_txn.add_value(1);

    // `unwrapped_entries` is built only to turn `entry.version_name`
    // (of type `Option<CommitSyncConfigVersion>`) into `entry.version_name.0`
    // (of type `Option<String>`). We need to do this, so that below we can
    // get `&Option<String>` for `insert_entries` vec
    let unwrapped_entries: Vec<_> = entries
        .into_iter()
        .map(|entry| {
            (
                entry.large_repo_id,
                entry.large_bcs_id,
                entry.small_repo_id,
                entry.small_bcs_id,
                entry.version_name.map(|vn| vn.0),
            )
        })
        .collect();

    let insert_entries: Vec<_> = unwrapped_entries
        .iter()
        .map(
            |(large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, version_name)| {
                (
                    large_repo_id,
                    large_bcs_id,
                    small_repo_id,
                    small_bcs_id,
                    version_name,
                )
            },
        )
        .collect();

    InsertMapping::query_with_transaction(txn, &insert_entries).and_then(move |(txn, _result)| {
        // We have to create temp_wce_entries because InsertWorkingCopyEquivalence requires
        // and array of references.
        let temp_wce_entries: Vec<_> = unwrapped_entries
            .iter()
            .map(
                |(large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, version_name)| {
                    (
                        *large_repo_id,
                        *large_bcs_id,
                        *small_repo_id,
                        Some(*small_bcs_id),
                        version_name.clone(),
                    )
                },
            )
            .collect();
        let wce_entries: Vec<_> = temp_wce_entries
            .iter()
            .map(
                |(large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, version_name)| {
                    (
                        large_repo_id,
                        large_bcs_id,
                        small_repo_id,
                        small_bcs_id,
                        version_name,
                    )
                },
            )
            .collect();

        InsertWorkingCopyEquivalence::query_with_transaction(txn, &wce_entries)
            .map(|(txn, result)| (txn, result.affected_rows()))
    })
}
