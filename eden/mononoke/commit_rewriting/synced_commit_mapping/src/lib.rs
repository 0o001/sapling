/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use sql::{Connection, Transaction};
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::SqlConnections;

use anyhow::Error;
use auto_impl::auto_impl;
use cloned::cloned;
use context::CoreContext;
use futures::compat::Future01CompatExt;
use futures::future::{FutureExt, TryFutureExt};
use futures_ext::{BoxFuture, FutureExt as _};
use futures_old::Future as Future01;
use metaconfig_types::CommitSyncConfigVersion;
use mononoke_types::{ChangesetId, RepositoryId};
use sql::mysql_async::{
    prelude::{ConvIr, FromValue},
    FromValueError, Value,
};
use sql::{mysql, queries};
use stats::prelude::*;
use thiserror::Error;

#[derive(Debug, Eq, Error, PartialEq)]
pub enum ErrorKind {
    #[error(
        "tried to insert inconsistent small bcs id {actual_bcs_id:?} version {actual_config_version:?}, while db has {expected_bcs_id:?} version {expected_config_version:?}"
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

// Repo that originally contained the synced commit
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, mysql::OptTryFromRowField)]
pub enum SyncedCommitSourceRepo {
    Large,
    Small,
}

impl ConvIr<SyncedCommitSourceRepo> for SyncedCommitSourceRepo {
    fn new(v: Value) -> Result<Self, FromValueError> {
        use SyncedCommitSourceRepo::*;

        match v {
            Value::Bytes(ref b) if b == b"large" => Ok(Large),
            Value::Bytes(ref b) if b == b"small" => Ok(Small),
            v => Err(FromValueError(v)),
        }
    }

    fn commit(self) -> SyncedCommitSourceRepo {
        self
    }

    fn rollback(self) -> Value {
        self.into()
    }
}

impl FromValue for SyncedCommitSourceRepo {
    type Intermediate = SyncedCommitSourceRepo;
}

impl From<SyncedCommitSourceRepo> for Value {
    fn from(source_repo: SyncedCommitSourceRepo) -> Self {
        use SyncedCommitSourceRepo::*;

        match source_repo {
            Small => Value::Bytes(b"small".to_vec()),
            Large => Value::Bytes(b"large".to_vec()),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SyncedCommitMappingEntry {
    pub large_repo_id: RepositoryId,
    pub large_bcs_id: ChangesetId,
    pub small_repo_id: RepositoryId,
    pub small_bcs_id: ChangesetId,
    pub version_name: Option<CommitSyncConfigVersion>,
    pub source_repo: Option<SyncedCommitSourceRepo>,
}

impl SyncedCommitMappingEntry {
    pub fn new(
        large_repo_id: RepositoryId,
        large_bcs_id: ChangesetId,
        small_repo_id: RepositoryId,
        small_bcs_id: ChangesetId,
        version_name: CommitSyncConfigVersion,
        source_repo: SyncedCommitSourceRepo,
    ) -> Self {
        Self {
            large_repo_id,
            large_bcs_id,
            small_repo_id,
            small_bcs_id,
            version_name: Some(version_name),
            source_repo: Some(source_repo),
        }
    }

    fn into_equivalent_working_copy_entry(self) -> EquivalentWorkingCopyEntry {
        let Self {
            large_repo_id,
            large_bcs_id,
            small_repo_id,
            small_bcs_id,
            version_name,
            source_repo: _,
        } = self;

        EquivalentWorkingCopyEntry {
            large_repo_id,
            large_bcs_id,
            small_repo_id,
            small_bcs_id: Some(small_bcs_id),
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

#[auto_impl(Arc)]
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
    ) -> BoxFuture<
        Vec<(
            ChangesetId,
            Option<CommitSyncConfigVersion>,
            Option<SyncedCommitSourceRepo>,
        )>,
        Error,
    >;

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
        sync_map_version_name: Option<CommitSyncConfigVersion>,
        source_repo: Option<SyncedCommitSourceRepo>,
    )) {
        insert_or_ignore,
        "{insert_or_ignore} INTO synced_commit_mapping (large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name, source_repo) VALUES {values}"
    }

    read SelectMapping(
        source_repo_id: RepositoryId,
        bcs_id: ChangesetId,
        target_repo_id: RepositoryId,
    ) -> (RepositoryId, ChangesetId, RepositoryId, ChangesetId, Option<CommitSyncConfigVersion>, Option<SyncedCommitSourceRepo>) {
        "SELECT large_repo_id, large_bcs_id, small_repo_id, small_bcs_id, sync_map_version_name, source_repo
          FROM synced_commit_mapping
          WHERE (large_repo_id = {source_repo_id} AND large_bcs_id = {bcs_id} AND small_repo_id = {target_repo_id}) OR
          (small_repo_id = {source_repo_id} AND small_bcs_id = {bcs_id} AND large_repo_id = {target_repo_id})"
    }

    write InsertWorkingCopyEquivalence(values: (
        large_repo_id: RepositoryId,
        large_bcs_id: ChangesetId,
        small_repo_id: RepositoryId,
        small_bcs_id: Option<ChangesetId>,
        sync_map_version_name: Option<CommitSyncConfigVersion>,
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
    ) -> (RepositoryId, ChangesetId, RepositoryId, Option<ChangesetId>, Option<CommitSyncConfigVersion>) {
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
    ) -> impl Future01<Item = u64, Error = Error> {
        let conn = self.write_connection.clone();
        async move {
            let txn = conn.start_transaction().await?;
            let (txn, affected_rows) = add_many_in_txn(txn, entries).await?;
            txn.commit().await?;
            Ok(affected_rows)
        }
        .boxed()
        .compat()
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
    ) -> BoxFuture<
        Vec<(
            ChangesetId,
            Option<CommitSyncConfigVersion>,
            Option<SyncedCommitSourceRepo>,
        )>,
        Error,
    > {
        STATS::gets.add_value(1);

        cloned!(self.read_connection, self.read_master_connection);
        async move {
            let rows =
                SelectMapping::query(&read_connection, &source_repo_id, &bcs_id, &target_repo_id)
                    .await?;

            let rows = if rows.is_empty() {
                STATS::gets_master.add_value(1);
                SelectMapping::query(
                    &read_master_connection,
                    &source_repo_id,
                    &bcs_id,
                    &target_repo_id,
                )
                .await?
            } else {
                rows
            };

            Ok(rows
                .into_iter()
                .map(|row| {
                    let (
                        large_repo_id,
                        large_bcs_id,
                        _small_repo_id,
                        small_bcs_id,
                        maybe_version_name,
                        maybe_source_repo,
                    ) = row;
                    if target_repo_id == large_repo_id {
                        (large_bcs_id, maybe_version_name, maybe_source_repo)
                    } else {
                        (small_bcs_id, maybe_version_name, maybe_source_repo)
                    }
                })
                .collect())
        }
        .boxed()
        .compat()
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
        cloned!(self.write_connection, ctx);
        async move {
            let result = InsertWorkingCopyEquivalence::query(
                &write_connection,
                &[(
                    &large_repo_id,
                    &large_bcs_id,
                    &small_repo_id,
                    &small_bcs_id,
                    &version_name,
                )],
            )
            .await?;

            if result.affected_rows() == 1 {
                Ok(true)
            } else {
                // Check that db stores consistent entry
                let maybe_equivalent_wc = this
                    .get_equivalent_working_copy(ctx, large_repo_id, large_bcs_id, small_repo_id)
                    .compat()
                    .await?;

                if let Some(equivalent_wc) = maybe_equivalent_wc {
                    use WorkingCopyEquivalence::*;
                    let (expected_bcs_id, expected_version) = match equivalent_wc {
                        WorkingCopy(wc, mapping) => (Some(wc), mapping),
                        NoWorkingCopy(mapping) => (None, mapping),
                    };
                    if (expected_bcs_id != small_bcs_id) || (expected_version != version_name_clone)
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
            }
        }
        .boxed()
        .compat()
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

        cloned!(self.read_master_connection, self.read_connection);
        async move {
            let rows = SelectWorkingCopyEquivalence::query(
                &read_connection,
                &source_repo_id,
                &source_bcs_id,
                &target_repo_id,
            )
            .await?;
            let maybe_row = if !rows.is_empty() {
                rows.get(0).cloned()
            } else {
                SelectWorkingCopyEquivalence::query(
                    &read_master_connection,
                    &source_repo_id,
                    &source_bcs_id,
                    &target_repo_id,
                )
                .await
                .map(|rows| rows.get(0).cloned())?
            };

            Ok(match maybe_row {
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
                            maybe_mapping,
                        ))
                    } else {
                        match maybe_small_bcs_id {
                            Some(small_bcs_id) => Some(WorkingCopyEquivalence::WorkingCopy(
                                small_bcs_id,
                                maybe_mapping,
                            )),
                            None => Some(WorkingCopyEquivalence::NoWorkingCopy(maybe_mapping)),
                        }
                    }
                }
                None => None,
            })
        }
        .boxed()
        .compat()
        .boxify()
    }
}

pub async fn add_many_in_txn(
    txn: Transaction,
    entries: Vec<SyncedCommitMappingEntry>,
) -> Result<(Transaction, u64), Error> {
    STATS::add_many_in_txn.add_value(1);

    let insert_entries: Vec<_> = entries
        .iter()
        .map(|entry| {
            (
                &entry.large_repo_id,
                &entry.large_bcs_id,
                &entry.small_repo_id,
                &entry.small_bcs_id,
                &entry.version_name,
                &entry.source_repo,
            )
        })
        .collect();

    let (txn, _result) = InsertMapping::query_with_transaction(txn, &insert_entries).await?;
    let owned_entries: Vec<_> = entries
        .into_iter()
        .map(|entry| entry.into_equivalent_working_copy_entry())
        .collect();

    let ref_entries: Vec<_> = owned_entries
        .iter()
        .map(|entry| {
            (
                &entry.large_repo_id,
                &entry.large_bcs_id,
                &entry.small_repo_id,
                &entry.small_bcs_id,
                &entry.version_name,
            )
        })
        .collect();

    let (txn, result) =
        InsertWorkingCopyEquivalence::query_with_transaction(txn, &ref_entries).await?;
    Ok((txn, result.affected_rows()))
}
