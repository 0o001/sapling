/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use abomonation_derive::Abomonation;
use anyhow::{Error, Result};
use async_trait::async_trait;
use auto_impl::auto_impl;
use bytes::Bytes;
use context::{CoreContext, PerfCounterType};
use fbinit::FacebookInit;
use fbthrift::compact_protocol;
use futures::{
    stream::{self, BoxStream, StreamExt},
    TryFutureExt,
};
use mononoke_types::{
    ChangesetId, ChangesetIdPrefix, ChangesetIdsResolvedFromPrefix, RepositoryId,
};
use rand::Rng;
use rendezvous::{RendezVous, RendezVousOptions, RendezVousStats, TunablesRendezVousController};
use sql::{queries, Connection, Transaction};
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::SqlConnections;
use stats::prelude::*;
use std::collections::{HashMap, HashSet};
use std::result;
use std::sync::Arc;

mod caching;
mod errors;
#[cfg(test)]
mod test;

pub use caching::{get_cache_key, CachingChangesets};
pub use errors::ErrorKind;

define_stats! {
    prefix = "mononoke.changesets";
    gets: timeseries(Rate, Sum),
    gets_master: timeseries(Rate, Sum),
    get_many_by_prefix: timeseries(Rate, Sum),
    adds: timeseries(Rate, Sum),
}

#[derive(Abomonation, Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChangesetEntry {
    pub repo_id: RepositoryId,
    pub cs_id: ChangesetId,
    pub parents: Vec<ChangesetId>,
    pub gen: u64,
}

impl ChangesetEntry {
    fn from_thrift(thrift_entry: changeset_entry_thrift::ChangesetEntry) -> Result<Self> {
        let parents: Result<Vec<_>> = thrift_entry
            .parents
            .into_iter()
            .map(ChangesetId::from_thrift)
            .collect();

        Ok(Self {
            repo_id: RepositoryId::new(thrift_entry.repo_id.0),
            cs_id: ChangesetId::from_thrift(thrift_entry.cs_id)?,
            parents: parents?,
            gen: thrift_entry.gen.0 as u64,
        })
    }

    fn into_thrift(self) -> changeset_entry_thrift::ChangesetEntry {
        changeset_entry_thrift::ChangesetEntry {
            repo_id: changeset_entry_thrift::RepoId(self.repo_id.id()),
            cs_id: self.cs_id.into_thrift(),
            parents: self.parents.into_iter().map(|p| p.into_thrift()).collect(),
            gen: changeset_entry_thrift::GenerationNum(self.gen as i64),
        }
    }
}

pub fn serialize_cs_entries(cs_entries: Vec<ChangesetEntry>) -> Bytes {
    let mut thrift_entries = vec![];
    for entry in cs_entries {
        let thrift_entry = changeset_entry_thrift::ChangesetEntry {
            repo_id: changeset_entry_thrift::RepoId(entry.repo_id.id()),
            cs_id: entry.cs_id.into_thrift(),
            parents: entry.parents.into_iter().map(|p| p.into_thrift()).collect(),
            gen: changeset_entry_thrift::GenerationNum(entry.gen as i64),
        };
        thrift_entries.push(thrift_entry);
    }

    compact_protocol::serialize(&thrift_entries)
}

pub fn deserialize_cs_entries(blob: &Bytes) -> Result<Vec<ChangesetEntry>> {
    let thrift_entries: Vec<changeset_entry_thrift::ChangesetEntry> =
        compact_protocol::deserialize(blob)?;
    let mut entries = vec![];
    for thrift_entry in thrift_entries {
        let parents = thrift_entry
            .parents
            .into_iter()
            .map(ChangesetId::from_thrift)
            .collect::<Result<Vec<_>>>()?;
        let entry = ChangesetEntry {
            repo_id: RepositoryId::new(thrift_entry.repo_id.0),
            cs_id: ChangesetId::from_thrift(thrift_entry.cs_id)?,
            parents,
            gen: thrift_entry.gen.0 as u64,
        };
        entries.push(entry);
    }
    Ok(entries)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ChangesetInsert {
    pub cs_id: ChangesetId,
    pub parents: Vec<ChangesetId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

/// Interface to storage of changesets that have been completely stored in Mononoke.
#[facet::facet]
#[async_trait]
#[auto_impl(&, Arc)]
pub trait Changesets: Send + Sync {
    /// The repository this `Changesets` is for.
    fn repo_id(&self) -> RepositoryId;

    /// Add a new entry to the changesets table. Returns true if new changeset was inserted,
    /// returns false if the same changeset has already existed.
    async fn add(&self, ctx: CoreContext, cs: ChangesetInsert) -> Result<bool, Error>;

    /// Retrieve the row specified by this commit, if available.
    async fn get(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> Result<Option<ChangesetEntry>, Error>;

    /// Retrieve the rows for all the commits if available
    async fn get_many(
        &self,
        ctx: CoreContext,
        cs_ids: Vec<ChangesetId>,
    ) -> Result<Vec<ChangesetEntry>, Error>;

    /// Retrieve the rows for all the commits with the given prefix up to the given limit
    async fn get_many_by_prefix(
        &self,
        ctx: CoreContext,
        cs_prefix: ChangesetIdPrefix,
        limit: usize,
    ) -> Result<ChangesetIdsResolvedFromPrefix, Error>;

    /// Prime any caches with known changeset entries.  The changeset entries
    /// must be for the repository associated with this `Changesets`.
    fn prime_cache(&self, ctx: &CoreContext, changesets: &[ChangesetEntry]);

    /// Enumerate all public changesets in the repository.
    ///
    /// This retruns a pair of unique integers that are the minimum and
    /// maxiumum unique changeset ids for this repository.
    ///
    /// This range can be used in subsequent calls to `list_enumeration_range`
    /// to enumerate the changesets.
    async fn enumeration_bounds(
        &self,
        ctx: &CoreContext,
        read_from_master: bool,
    ) -> Result<Option<(u64, u64)>>;

    /// Enumerate a range of public changesets in the repository.
    ///
    /// This lists all changesets in the given range of unique integer ids
    /// that belong to this repositories, along with their unique integer ids.
    /// Unique ids are assigned for all changesets (public or draft) in all
    /// repositories, so a given range may not have any changesets for this
    /// repository.
    ///
    /// The results can optionally be sorted and limited so that enumeration
    /// can be performed in chunks for repositories with large numbers of
    /// commits.
    ///
    /// Use `enumeration_bounds` to find suitable starting values for
    /// `min_id` and `max_id`.
    fn list_enumeration_range(
        &self,
        ctx: &CoreContext,
        min_id: u64,
        max_id: u64,
        sort_and_limit: Option<(SortOrder, u64)>,
        read_from_master: bool,
    ) -> BoxStream<'_, Result<(ChangesetId, u64), Error>>;
}

#[derive(Clone)]
struct RendezVousConnection {
    rdv: RendezVous<ChangesetId, ChangesetEntry>,
    conn: Connection,
}

impl RendezVousConnection {
    fn new(conn: Connection, name: &str, opts: RendezVousOptions) -> Self {
        Self {
            conn,
            rdv: RendezVous::new(
                TunablesRendezVousController::new(opts),
                Arc::new(RendezVousStats::new(format!("changesets.{}", name,))),
            ),
        }
    }
}

#[derive(Clone)]
pub struct SqlChangesets {
    repo_id: RepositoryId,
    write_connection: Connection,
    read_connection: RendezVousConnection,
    read_master_connection: RendezVousConnection,
}

queries! {
    write InsertChangeset(values: (repo_id: RepositoryId, cs_id: ChangesetId, gen: u64)) {
        insert_or_ignore,
        "{insert_or_ignore} INTO changesets (repo_id, cs_id, gen) VALUES {values}"
    }

    write InsertParents(values: (cs_id: u64, parent_id: u64, seq: i32)) {
        none,
        "INSERT INTO csparents (cs_id, parent_id, seq) VALUES {values}"
    }

    read SelectChangeset(repo_id: RepositoryId, cs_id: ChangesetId, tok: i32) -> (u64, Option<ChangesetId>, Option<u64>, i32) {
        // NOTE: This selects seq even though we don't need it in order to sort by it.
        "
        SELECT cs0.gen AS gen, cs1.cs_id AS parent_id, csparents.seq AS seq, {tok}
        FROM csparents
        INNER JOIN changesets cs0 ON cs0.id = csparents.cs_id
        INNER JOIN changesets cs1 ON cs1.id = csparents.parent_id
        WHERE cs0.repo_id = {repo_id} AND cs0.cs_id = {cs_id} AND cs1.repo_id = {repo_id}

        UNION

        SELECT cs0.gen AS gen, NULL AS parent_id, NULL as seq, {tok}
        FROM changesets cs0
        WHERE cs0.repo_id = {repo_id} and cs0.cs_id = {cs_id}

        ORDER BY seq ASC
        "
    }

    read SelectManyChangesets(repo_id: RepositoryId, tok: i32, >list cs_id: ChangesetId) -> (ChangesetId, u64, Option<ChangesetId>, Option<u64>, i32) {
        "
        SELECT cs0.cs_id AS cs_id, cs0.gen AS gen, cs1.cs_id AS parent_id, csparents.seq AS seq, {tok}
        FROM csparents
        INNER JOIN changesets cs0 ON cs0.id = csparents.cs_id
        INNER JOIN changesets cs1 ON cs1.id = csparents.parent_id
        WHERE cs0.repo_id = {repo_id} AND cs0.cs_id IN {cs_id} AND cs1.repo_id = {repo_id}

        UNION

        SELECT cs0.cs_id AS cs_id, cs0.gen AS gen, NULL AS parent_id, NULL as seq, {tok}
        FROM changesets cs0
        WHERE cs0.repo_id = {repo_id} and cs0.cs_id IN {cs_id}

        ORDER BY seq ASC
        "
    }

    read SelectChangesets(repo_id: RepositoryId, >list cs_id: ChangesetId) -> (u64, ChangesetId, u64) {
        "SELECT id, cs_id, gen
         FROM changesets
         WHERE repo_id = {repo_id}
           AND cs_id IN {cs_id}"
    }

    read SelectChangesetsRange(repo_id: RepositoryId, min: &[u8], max: &[u8], limit: usize) -> (ChangesetId) {
        "SELECT cs_id
         FROM changesets
         WHERE repo_id = {repo_id}
           AND cs_id >= {min} AND cs_id <= {max}
           LIMIT {limit}
        "
    }

    read SelectAllChangesetsIdsInRange(repo_id: RepositoryId, min_id: u64, max_id: u64) -> (ChangesetId, u64) {
        mysql(
            "SELECT cs_id, id
            FROM changesets FORCE INDEX(repo_id_id)
            WHERE repo_id = {repo_id}
            AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id"
        )
        sqlite(
            "SELECT cs_id, id
            FROM changesets
            WHERE repo_id = {repo_id}
            AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id"
        )
    }

    read SelectAllChangesetsIdsInRangeLimitAsc(repo_id: RepositoryId, min_id: u64, max_id: u64, limit: u64) -> (ChangesetId, u64) {
        mysql(
            "SELECT cs_id, id
            FROM changesets FORCE INDEX(repo_id_id)
            WHERE repo_id = {repo_id}
            AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id
            LIMIT {limit}"
        )
        sqlite(
            "SELECT cs_id, id
            FROM changesets
            WHERE repo_id = {repo_id}
            AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id
            LIMIT {limit}"
        )
    }

    read SelectAllChangesetsIdsInRangeLimitDesc(repo_id: RepositoryId, min_id: u64, max_id: u64, limit: u64) -> (ChangesetId, u64) {
        mysql(
            "SELECT cs_id, id
            FROM changesets FORCE INDEX(repo_id_id)
            WHERE repo_id = {repo_id}
              AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id DESC
            LIMIT {limit}"
        )
        sqlite(
            "SELECT cs_id, id
            FROM changesets
            WHERE repo_id = {repo_id}
              AND id BETWEEN {min_id} AND {max_id}
            ORDER BY id DESC
            LIMIT {limit}"
        )
    }

    read SelectChangesetsIdsBounds(repo_id: RepositoryId) -> (u64, u64) {
        "SELECT min(id), max(id)
         FROM changesets
         WHERE repo_id = {repo_id}"
    }

}

#[derive(Clone)]
pub struct SqlChangesetsBuilder {
    connections: SqlConnections,
}

impl SqlConstruct for SqlChangesetsBuilder {
    const LABEL: &'static str = "changesets";

    const CREATION_QUERY: &'static str = include_str!("../schemas/sqlite-changesets.sql");

    fn from_sql_connections(connections: SqlConnections) -> Self {
        Self { connections }
    }
}

impl SqlConstructFromMetadataDatabaseConfig for SqlChangesetsBuilder {}

impl SqlChangesetsBuilder {
    pub fn build(self, opts: RendezVousOptions, repo_id: RepositoryId) -> SqlChangesets {
        let SqlConnections {
            read_connection,
            read_master_connection,
            write_connection,
        } = self.connections;

        SqlChangesets {
            repo_id,
            read_connection: RendezVousConnection::new(read_connection, "read", opts),
            read_master_connection: RendezVousConnection::new(
                read_master_connection,
                "read_master",
                opts,
            ),
            write_connection,
        }
    }
}

#[async_trait]
impl Changesets for SqlChangesets {
    fn repo_id(&self) -> RepositoryId {
        self.repo_id
    }

    async fn add(&self, ctx: CoreContext, cs: ChangesetInsert) -> Result<bool, Error> {
        STATS::adds.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlWrites);

        let parent_rows = {
            if cs.parents.is_empty() {
                Vec::new()
            } else {
                SelectChangesets::query(&self.write_connection, &self.repo_id, &cs.parents[..])
                    .await?
            }
        };
        check_missing_rows(&cs.parents, &parent_rows)?;
        let gen = parent_rows.iter().map(|row| row.2).max().unwrap_or(0) + 1;
        let transaction = self.write_connection.start_transaction().await?;
        let (transaction, result) = InsertChangeset::query_with_transaction(
            transaction,
            &[(&self.repo_id, &cs.cs_id, &gen)],
        )
        .await?;

        if result.affected_rows() == 1 && result.last_insert_id().is_some() {
            insert_parents(
                transaction,
                result.last_insert_id().unwrap(),
                cs,
                parent_rows,
            )
            .await?;
            Ok(true)
        } else {
            transaction.rollback().await?;
            check_changeset_matches(&self.write_connection, self.repo_id, cs).await?;
            Ok(false)
        }
    }

    async fn get(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> Result<Option<ChangesetEntry>, Error> {
        let res = self.get_many(ctx, vec![cs_id]).await?.into_iter().next();
        Ok(res)
    }

    async fn get_many(
        &self,
        ctx: CoreContext,
        cs_ids: Vec<ChangesetId>,
    ) -> Result<Vec<ChangesetEntry>, Error> {
        if cs_ids.is_empty() {
            return Ok(vec![]);
        }
        STATS::gets.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlReadsReplica);

        let fetched_cs =
            select_many_changesets(ctx.fb, &self.read_connection, self.repo_id, &cs_ids).await?;
        let fetched_set: HashSet<_> = fetched_cs
            .clone()
            .into_iter()
            .map(|cs_entry| cs_entry.cs_id)
            .collect();

        let notfetched_cs_ids: Vec<_> = cs_ids
            .into_iter()
            .filter(|cs_id| !fetched_set.contains(cs_id))
            .collect();
        if notfetched_cs_ids.is_empty() {
            Ok(fetched_cs)
        } else {
            STATS::gets_master.add_value(1);
            ctx.perf_counters()
                .increment_counter(PerfCounterType::SqlReadsMaster);
            let mut master_fetched_cs = select_many_changesets(
                ctx.fb,
                &self.read_master_connection,
                self.repo_id,
                &notfetched_cs_ids,
            )
            .await?;
            master_fetched_cs.extend(fetched_cs);
            Ok(master_fetched_cs)
        }
    }

    async fn get_many_by_prefix(
        &self,
        ctx: CoreContext,
        cs_prefix: ChangesetIdPrefix,
        limit: usize,
    ) -> Result<ChangesetIdsResolvedFromPrefix, Error> {
        STATS::get_many_by_prefix.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlReadsReplica);
        let resolved_cs =
            fetch_many_by_prefix(&self.read_connection.conn, self.repo_id, &cs_prefix, limit)
                .await?;
        match resolved_cs {
            ChangesetIdsResolvedFromPrefix::NoMatch => {
                ctx.perf_counters()
                    .increment_counter(PerfCounterType::SqlReadsMaster);
                fetch_many_by_prefix(
                    &self.read_master_connection.conn,
                    self.repo_id,
                    &cs_prefix,
                    limit,
                )
                .await
            }
            _ => Ok(resolved_cs),
        }
    }

    fn prime_cache(&self, _ctx: &CoreContext, _changesets: &[ChangesetEntry]) {
        // No-op
    }

    async fn enumeration_bounds(
        &self,
        _ctx: &CoreContext,
        read_from_master: bool,
    ) -> Result<Option<(u64, u64)>, Error> {
        let conn = self.read_conn(read_from_master);
        let rows = SelectChangesetsIdsBounds::query(conn, &self.repo_id).await?;
        if rows.is_empty() {
            Ok(None)
        } else {
            Ok(Some((rows[0].0, rows[0].1)))
        }
    }

    fn list_enumeration_range(
        &self,
        _ctx: &CoreContext,
        min_id: u64,
        max_id: u64,
        sort_and_limit: Option<(SortOrder, u64)>,
        read_from_master: bool,
    ) -> BoxStream<'_, Result<(ChangesetId, u64), Error>> {
        // We expect the range [min_id, max_id), so subtract 1 from max_id as
        // SQL request is BETWEEN, which means both bounds are inclusive.
        let max_id = max_id - 1;
        let conn = self.read_conn(read_from_master);

        async move {
            match sort_and_limit {
                None => {
                    SelectAllChangesetsIdsInRange::query(&conn, &self.repo_id, &min_id, &max_id)
                        .await
                }
                Some((SortOrder::Ascending, limit)) => {
                    SelectAllChangesetsIdsInRangeLimitAsc::query(
                        &conn,
                        &self.repo_id,
                        &min_id,
                        &max_id,
                        &limit,
                    )
                    .await
                }
                Some((SortOrder::Descending, limit)) => {
                    SelectAllChangesetsIdsInRangeLimitDesc::query(
                        &conn,
                        &self.repo_id,
                        &min_id,
                        &max_id,
                        &limit,
                    )
                    .await
                }
            }
        }
        .map_ok(|rows| {
            let changesets_ids = rows.into_iter().map(|row| Ok((row.0, row.1)));
            stream::iter(changesets_ids)
        })
        .try_flatten_stream()
        .boxed()
    }
}

async fn fetch_many_by_prefix(
    connection: &Connection,
    repo_id: RepositoryId,
    cs_prefix: &ChangesetIdPrefix,
    limit: usize,
) -> Result<ChangesetIdsResolvedFromPrefix, Error> {
    let rows = SelectChangesetsRange::query(
        &connection,
        &repo_id,
        &cs_prefix.min_as_ref(),
        &cs_prefix.max_as_ref(),
        &(limit + 1),
    )
    .await?;
    let mut fetched_cs: Vec<ChangesetId> = rows.into_iter().map(|row| row.0).collect();
    let result = match fetched_cs.len() {
        0 => ChangesetIdsResolvedFromPrefix::NoMatch,
        1 => ChangesetIdsResolvedFromPrefix::Single(fetched_cs[0].clone()),
        l if l <= limit => ChangesetIdsResolvedFromPrefix::Multiple(fetched_cs),
        _ => ChangesetIdsResolvedFromPrefix::TooMany({
            fetched_cs.pop();
            fetched_cs
        }),
    };
    Ok(result)
}

impl SqlChangesets {
    fn read_conn(&self, read_from_master: bool) -> &Connection {
        if read_from_master {
            &self.read_master_connection.conn
        } else {
            &self.read_connection.conn
        }
    }
}

fn check_missing_rows(
    expected: &[ChangesetId],
    actual: &[(u64, ChangesetId, u64)],
) -> result::Result<(), ErrorKind> {
    // Could just count the number here and report an error if any are missing, but the reporting
    // wouldn't be as nice.
    let expected_set: HashSet<_> = expected.iter().collect();
    let actual_set: HashSet<_> = actual.iter().map(|row| &row.1).collect();
    let diff = &expected_set - &actual_set;
    if diff.is_empty() {
        Ok(())
    } else {
        Err(ErrorKind::MissingParents(
            diff.into_iter().map(|cs_id| *cs_id).collect(),
        ))
    }
}

async fn insert_parents(
    transaction: Transaction,
    new_cs_id: u64,
    cs: ChangesetInsert,
    parent_rows: Vec<(u64, ChangesetId, u64)>,
) -> Result<(), Error> {
    // parent_rows might not be in the same order as cs.parents.
    let parent_map: HashMap<_, _> = parent_rows.into_iter().map(|row| (row.1, row.0)).collect();

    // enumerate() would be OK here too, but involve conversions from usize
    // to i32 within the map function.
    let parent_inserts: Vec<_> = (0..(cs.parents.len() as i32))
        .zip(cs.parents.iter())
        .map(|(seq, parent)| {
            // check_missing_rows should have ensured that all the IDs are
            // present.
            let parent_id = parent_map
                .get(&parent)
                .expect("check_missing_rows check failed");

            (new_cs_id, *parent_id, seq)
        })
        .collect();

    let ref_parent_inserts: Vec<_> = parent_inserts
        .iter()
        .map(|row| (&row.0, &row.1, &row.2))
        .collect();

    let (transaction, _) =
        InsertParents::query_with_transaction(transaction, &ref_parent_inserts[..]).await?;
    transaction.commit().await?;
    Ok(())
}

async fn check_changeset_matches(
    connection: &Connection,
    repo_id: RepositoryId,
    cs: ChangesetInsert,
) -> Result<(), Error> {
    let stored_parents = select_changeset(&connection, repo_id, cs.cs_id)
        .await?
        .map(|cs| cs.parents);
    if Some(&cs.parents) == stored_parents.as_ref() {
        Ok(())
    } else {
        Err(ErrorKind::DuplicateInsertionInconsistency(
            cs.cs_id,
            stored_parents.unwrap_or_default(),
            cs.parents,
        )
        .into())
    }
}

async fn select_changeset(
    connection: &Connection,
    repo_id: RepositoryId,
    cs_id: ChangesetId,
) -> Result<Option<ChangesetEntry>, Error> {
    let tok: i32 = rand::thread_rng().gen();
    let rows = SelectChangeset::query(&connection, &repo_id, &cs_id, &tok).await?;
    let result = if rows.is_empty() {
        None
    } else {
        let gen = rows[0].0;
        Some(ChangesetEntry {
            repo_id,
            cs_id,
            parents: rows.into_iter().filter_map(|row| row.1).collect(),
            gen,
        })
    };
    Ok(result)
}

async fn select_many_changesets(
    fb: FacebookInit,
    connection: &RendezVousConnection,
    repo_id: RepositoryId,
    cs_ids: &Vec<ChangesetId>,
) -> Result<Vec<ChangesetEntry>, Error> {
    if cs_ids.is_empty() {
        return Ok(vec![]);
    }

    let ret = connection
        .rdv
        .dispatch(fb, cs_ids.iter().copied().collect(), || {
            let conn = connection.conn.clone();
            move |cs_ids| async move {
                let cs_ids = cs_ids.into_iter().collect::<Vec<_>>();

                let tok: i32 = rand::thread_rng().gen();

                let fetched_changesets =
                    SelectManyChangesets::query(&conn, &repo_id, &tok, &cs_ids[..]).await?;

                let mut cs_id_to_cs_entry = HashMap::new();
                for (cs_id, gen, maybe_parent, _, _) in fetched_changesets {
                    cs_id_to_cs_entry
                        .entry(cs_id)
                        .or_insert(ChangesetEntry {
                            repo_id,
                            cs_id,
                            parents: vec![],
                            gen,
                        })
                        .parents
                        .extend(maybe_parent.into_iter());
                }

                Ok(cs_id_to_cs_entry)
            }
        })
        .await?;

    Ok(ret.into_iter().filter_map(|(_, v)| v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize() {
        let entry = ChangesetEntry {
            repo_id: RepositoryId::new(0),
            cs_id: mononoke_types_mocks::changesetid::ONES_CSID,
            parents: vec![mononoke_types_mocks::changesetid::TWOS_CSID],
            gen: 2,
        };

        let res = deserialize_cs_entries(&serialize_cs_entries(vec![entry.clone(), entry.clone()]))
            .unwrap();
        assert_eq!(vec![entry.clone(), entry], res);
    }
}
