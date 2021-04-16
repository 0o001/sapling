/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use async_trait::async_trait;
use auto_impl::auto_impl;

use sql::Connection;
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::SqlConnections;

use abomonation_derive::Abomonation;
use anyhow::{Error, Result};
use context::{CoreContext, PerfCounterType};
use fbinit::FacebookInit;
use futures::future;
use mercurial_types::{
    HgChangesetId, HgChangesetIdPrefix, HgChangesetIdsResolvedFromPrefix, HgNodeHash,
};
use mononoke_types::{ChangesetId, RepositoryId};
use rand::Rng;
use rendezvous::{
    MultiRendezVous, RendezVousOptions, RendezVousStats, TunablesMultiRendezVousController,
};
use sql::queries;
use stats::prelude::*;

mod caching;
mod errors;

pub use crate::caching::CachingBonsaiHgMapping;
pub use crate::errors::ErrorKind;

define_stats! {
    prefix = "mononoke.bonsai_hg_mapping";
    gets: timeseries(Rate, Sum),
    gets_master: timeseries(Rate, Sum),
    adds: timeseries(Rate, Sum),
    get_many_hg_by_prefix: timeseries(Rate, Sum),
}

#[derive(Abomonation, Clone, Debug, Eq, Hash, PartialEq)]
pub struct BonsaiHgMappingEntry {
    pub repo_id: RepositoryId,
    pub hg_cs_id: HgChangesetId,
    pub bcs_id: ChangesetId,
}

impl BonsaiHgMappingEntry {
    fn from_thrift(entry: bonsai_hg_mapping_entry_thrift::BonsaiHgMappingEntry) -> Result<Self> {
        Ok(Self {
            repo_id: RepositoryId::new(entry.repo_id.0),
            hg_cs_id: HgChangesetId::new(HgNodeHash::from_thrift(entry.hg_cs_id)?),
            bcs_id: ChangesetId::from_thrift(entry.bcs_id)?,
        })
    }

    fn into_thrift(self) -> bonsai_hg_mapping_entry_thrift::BonsaiHgMappingEntry {
        bonsai_hg_mapping_entry_thrift::BonsaiHgMappingEntry {
            repo_id: bonsai_hg_mapping_entry_thrift::RepoId(self.repo_id.id()),
            hg_cs_id: self.hg_cs_id.into_nodehash().into_thrift(),
            bcs_id: self.bcs_id.into_thrift(),
        }
    }

    pub fn new(repo_id: RepositoryId, hg_cs_id: HgChangesetId, bcs_id: ChangesetId) -> Self {
        BonsaiHgMappingEntry {
            repo_id,
            hg_cs_id,
            bcs_id,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum BonsaiOrHgChangesetIds {
    Bonsai(Vec<ChangesetId>),
    Hg(Vec<HgChangesetId>),
}

impl BonsaiOrHgChangesetIds {
    pub fn is_empty(&self) -> bool {
        match self {
            BonsaiOrHgChangesetIds::Bonsai(v) => v.is_empty(),
            BonsaiOrHgChangesetIds::Hg(v) => v.is_empty(),
        }
    }
}

impl From<ChangesetId> for BonsaiOrHgChangesetIds {
    fn from(cs_id: ChangesetId) -> Self {
        BonsaiOrHgChangesetIds::Bonsai(vec![cs_id])
    }
}

impl From<Vec<ChangesetId>> for BonsaiOrHgChangesetIds {
    fn from(cs_ids: Vec<ChangesetId>) -> Self {
        BonsaiOrHgChangesetIds::Bonsai(cs_ids)
    }
}

impl From<HgChangesetId> for BonsaiOrHgChangesetIds {
    fn from(cs_id: HgChangesetId) -> Self {
        BonsaiOrHgChangesetIds::Hg(vec![cs_id])
    }
}

impl From<Vec<HgChangesetId>> for BonsaiOrHgChangesetIds {
    fn from(cs_ids: Vec<HgChangesetId>) -> Self {
        BonsaiOrHgChangesetIds::Hg(cs_ids)
    }
}

#[facet::facet]
#[async_trait]
#[auto_impl(&, Arc, Box)]
pub trait BonsaiHgMapping: Send + Sync {
    async fn add(&self, ctx: &CoreContext, entry: BonsaiHgMappingEntry) -> Result<bool, Error>;

    async fn get(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        cs_id: BonsaiOrHgChangesetIds,
    ) -> Result<Vec<BonsaiHgMappingEntry>, Error>;

    async fn get_hg_from_bonsai(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> Result<Option<HgChangesetId>, Error> {
        let result = self.get(ctx, repo_id, cs_id.into()).await?;
        let hg_cs_id = result.into_iter().next().map(|entry| entry.hg_cs_id);
        Ok(hg_cs_id)
    }

    async fn get_bonsai_from_hg(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        cs_id: HgChangesetId,
    ) -> Result<Option<ChangesetId>, Error> {
        let result = self.get(ctx, repo_id, cs_id.into()).await?;
        let bcs_id = result.into_iter().next().map(|entry| entry.bcs_id);
        Ok(bcs_id)
    }

    async fn get_many_hg_by_prefix(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        cs_prefix: HgChangesetIdPrefix,
        limit: usize,
    ) -> Result<HgChangesetIdsResolvedFromPrefix, Error>;
}

#[derive(Clone)]
struct RendezVousConnection {
    bonsai: MultiRendezVous<RepositoryId, ChangesetId, HgChangesetId>,
    hg: MultiRendezVous<RepositoryId, HgChangesetId, ChangesetId>,
    conn: Connection,
}

impl RendezVousConnection {
    fn new(conn: Connection, name: &str, opts: RendezVousOptions) -> Self {
        Self {
            conn,
            bonsai: MultiRendezVous::new(
                TunablesMultiRendezVousController::new(opts),
                RendezVousStats::new(format!("bonsai_hg_mapping.bonsai.{}", name,)),
            ),
            hg: MultiRendezVous::new(
                TunablesMultiRendezVousController::new(opts),
                RendezVousStats::new(format!("bonsai_hg_mapping.hg.{}", name,)),
            ),
        }
    }
}

#[derive(Clone)]
pub struct SqlBonsaiHgMapping {
    write_connection: Connection,
    read_connection: RendezVousConnection,
    read_master_connection: RendezVousConnection,
}

queries! {
    write InsertMapping(values: (
        repo_id: RepositoryId,
        hg_cs_id: HgChangesetId,
        bcs_id: ChangesetId,
    )) {
        insert_or_ignore,
        "{insert_or_ignore} INTO bonsai_hg_mapping (repo_id, hg_cs_id, bcs_id) VALUES {values}"
    }

    read SelectMappingByBonsai(
        repo_id: RepositoryId,
        tok: i32,
        >list bcs_id: ChangesetId
    ) -> (HgChangesetId, ChangesetId, i32) {
        "SELECT hg_cs_id, bcs_id, {tok}
         FROM bonsai_hg_mapping
         WHERE repo_id = {repo_id}
           AND bcs_id IN {bcs_id}"
    }

    read SelectMappingByHg(
        repo_id: RepositoryId,
        tok: i32,
        >list hg_cs_id: HgChangesetId
    ) -> (HgChangesetId, ChangesetId, i32) {
        "SELECT hg_cs_id, bcs_id, {tok}
         FROM bonsai_hg_mapping
         WHERE repo_id = {repo_id}
           AND hg_cs_id IN {hg_cs_id}"
    }

    read SelectHgChangesetsByRange(repo_id: RepositoryId, hg_cs_min: &[u8], hg_cs_max: &[u8], limit: usize) -> (HgChangesetId) {
        "SELECT hg_cs_id
         FROM bonsai_hg_mapping
         WHERE repo_id = {repo_id}
           AND hg_cs_id >= {hg_cs_min} AND hg_cs_id <= {hg_cs_max}
           LIMIT {limit}
        "
    }
}

#[derive(Clone)]
pub struct SqlBonsaiHgMappingBuilder {
    connections: SqlConnections,
}

impl SqlConstruct for SqlBonsaiHgMappingBuilder {
    const LABEL: &'static str = "bonsai_hg_mapping";

    const CREATION_QUERY: &'static str = include_str!("../schemas/sqlite-bonsai-hg-mapping.sql");

    fn from_sql_connections(connections: SqlConnections) -> Self {
        Self { connections }
    }
}

impl SqlBonsaiHgMappingBuilder {
    pub fn build(self, opts: RendezVousOptions) -> SqlBonsaiHgMapping {
        let connections = self.connections;

        SqlBonsaiHgMapping {
            write_connection: connections.write_connection,
            read_connection: RendezVousConnection::new(connections.read_connection, "reader", opts),
            read_master_connection: RendezVousConnection::new(
                connections.read_master_connection,
                "read_master",
                opts,
            ),
        }
    }
}

impl SqlConstructFromMetadataDatabaseConfig for SqlBonsaiHgMappingBuilder {}

impl SqlBonsaiHgMapping {
    async fn verify_consistency(&self, entry: BonsaiHgMappingEntry) -> Result<(), Error> {
        let BonsaiHgMappingEntry {
            repo_id,
            hg_cs_id,
            bcs_id,
        } = entry.clone();

        let tok: i32 = rand::thread_rng().gen();
        let hg_ids = &[hg_cs_id];
        let by_hg =
            SelectMappingByHg::query(&self.read_master_connection.conn, &repo_id, &tok, hg_ids);
        let bcs_ids = &[bcs_id];
        let by_bcs = SelectMappingByBonsai::query(
            &self.read_master_connection.conn,
            &repo_id,
            &tok,
            bcs_ids,
        );

        let (by_hg_rows, by_bcs_rows) = future::try_join(by_hg, by_bcs).await?;

        match by_hg_rows
            .into_iter()
            .chain(by_bcs_rows.into_iter())
            .map(|(hg_cs_id, bcs_id, _)| (hg_cs_id, bcs_id))
            .next()
        {
            Some(entry) if entry == (hg_cs_id, bcs_id) => Ok(()),
            Some((hg_cs_id, bcs_id)) => Err(ErrorKind::ConflictingEntries(
                BonsaiHgMappingEntry {
                    repo_id,
                    hg_cs_id,
                    bcs_id,
                },
                entry,
            )
            .into()),
            None => Err(ErrorKind::RaceConditionWithDelete(entry).into()),
        }
    }
}

#[async_trait]
impl BonsaiHgMapping for SqlBonsaiHgMapping {
    async fn add(&self, ctx: &CoreContext, entry: BonsaiHgMappingEntry) -> Result<bool, Error> {
        STATS::adds.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlWrites);

        let BonsaiHgMappingEntry {
            repo_id,
            hg_cs_id,
            bcs_id,
        } = entry.clone();

        let result =
            InsertMapping::query(&self.write_connection, &[(&repo_id, &hg_cs_id, &bcs_id)]).await?;

        if result.affected_rows() == 1 {
            Ok(true)
        } else {
            self.verify_consistency(entry).await?;
            Ok(false)
        }
    }

    async fn get(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        ids: BonsaiOrHgChangesetIds,
    ) -> Result<Vec<BonsaiHgMappingEntry>, Error> {
        STATS::gets.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlReadsReplica);
        let (mut mappings, left_to_fetch) =
            select_mapping(ctx.fb, &self.read_connection, repo_id, ids).await?;

        if left_to_fetch.is_empty() {
            return Ok(mappings);
        }

        STATS::gets_master.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlReadsMaster);
        let (mut master_mappings, _) =
            select_mapping(ctx.fb, &self.read_master_connection, repo_id, left_to_fetch).await?;

        mappings.append(&mut master_mappings);
        Ok(mappings)
    }

    async fn get_many_hg_by_prefix(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        cs_prefix: HgChangesetIdPrefix,
        limit: usize,
    ) -> Result<HgChangesetIdsResolvedFromPrefix, Error> {
        STATS::get_many_hg_by_prefix.add_value(1);
        ctx.perf_counters()
            .increment_counter(PerfCounterType::SqlReadsReplica);
        let resolved_cs =
            fetch_many_hg_by_prefix(&self.read_connection.conn, repo_id, &cs_prefix, limit).await?;

        match resolved_cs {
            HgChangesetIdsResolvedFromPrefix::NoMatch => {
                ctx.perf_counters()
                    .increment_counter(PerfCounterType::SqlReadsMaster);
                fetch_many_hg_by_prefix(
                    &self.read_master_connection.conn,
                    repo_id,
                    &cs_prefix,
                    limit,
                )
                .await
            }
            _ => Ok(resolved_cs),
        }
    }
}

async fn fetch_many_hg_by_prefix(
    connection: &Connection,
    repo_id: RepositoryId,
    cs_prefix: &HgChangesetIdPrefix,
    limit: usize,
) -> Result<HgChangesetIdsResolvedFromPrefix, Error> {
    let rows = SelectHgChangesetsByRange::query(
        &connection,
        &repo_id,
        &cs_prefix.min_as_ref(),
        &cs_prefix.max_as_ref(),
        &(limit + 1),
    )
    .await?;

    let mut fetched_cs: Vec<HgChangesetId> = rows.into_iter().map(|row| row.0).collect();

    let res = match fetched_cs.len() {
        0 => HgChangesetIdsResolvedFromPrefix::NoMatch,
        1 => HgChangesetIdsResolvedFromPrefix::Single(fetched_cs[0].clone()),
        l if l <= limit => HgChangesetIdsResolvedFromPrefix::Multiple(fetched_cs),
        _ => HgChangesetIdsResolvedFromPrefix::TooMany({
            fetched_cs.pop();
            fetched_cs
        }),
    };

    Ok(res)
}

async fn select_mapping(
    fb: FacebookInit,
    connection: &RendezVousConnection,
    repo_id: RepositoryId,
    cs_ids: BonsaiOrHgChangesetIds,
) -> Result<(Vec<BonsaiHgMappingEntry>, BonsaiOrHgChangesetIds), Error> {
    if cs_ids.is_empty() {
        return Ok((vec![], cs_ids));
    }

    let tok: i32 = rand::thread_rng().gen();

    let (found, missing): (Vec<_>, _) = match cs_ids {
        BonsaiOrHgChangesetIds::Bonsai(bcs_ids) => {
            let ret = connection
                .bonsai
                .get(repo_id)
                .dispatch(fb, bcs_ids.into_iter().collect(), || {
                    let conn = connection.conn.clone();
                    move |bcs_ids| async move {
                        let bcs_ids = bcs_ids.into_iter().collect::<Vec<_>>();

                        Ok(
                            SelectMappingByBonsai::query(&conn, &repo_id, &tok, &bcs_ids[..])
                                .await?
                                .into_iter()
                                .map(|(hg_cs_id, bcs_id, _)| (bcs_id, hg_cs_id))
                                .collect(),
                        )
                    }
                })
                .await?;

            let mut not_found = vec![];
            let found = ret
                .into_iter()
                .filter_map(|(bcs_id, hg_cs_id)| match hg_cs_id {
                    Some(hg_cs_id) => Some((hg_cs_id, bcs_id)),
                    None => {
                        not_found.push(bcs_id);
                        None
                    }
                })
                .collect();

            (found, BonsaiOrHgChangesetIds::Bonsai(not_found))
        }
        BonsaiOrHgChangesetIds::Hg(hg_cs_ids) => {
            let ret = connection
                .hg
                .get(repo_id)
                .dispatch(fb, hg_cs_ids.into_iter().collect(), || {
                    let conn = connection.conn.clone();
                    move |hg_cs_ids| async move {
                        let hg_cs_ids = hg_cs_ids.into_iter().collect::<Vec<_>>();
                        Ok(
                            SelectMappingByHg::query(&conn, &repo_id, &tok, &hg_cs_ids[..])
                                .await?
                                .into_iter()
                                .map(|(hg_cs_id, bcs_id, _)| (hg_cs_id, bcs_id))
                                .collect(),
                        )
                    }
                })
                .await?;

            let mut not_found = vec![];
            let found = ret
                .into_iter()
                .filter_map(|(hg_cs_id, bcs_id)| match bcs_id {
                    Some(bcs_id) => Some((hg_cs_id, bcs_id)),
                    None => {
                        not_found.push(hg_cs_id);
                        None
                    }
                })
                .collect();

            (found, BonsaiOrHgChangesetIds::Hg(not_found))
        }
    };

    Ok((
        found
            .into_iter()
            .map(move |(hg_cs_id, bcs_id)| BonsaiHgMappingEntry {
                repo_id,
                hg_cs_id,
                bcs_id,
            })
            .collect(),
        missing,
    ))
}
