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
use std::collections::HashSet;
use thiserror::Error;

use anyhow::Error;
use cloned::cloned;
use context::CoreContext;
use futures::{
    compat::Future01CompatExt,
    future::{FutureExt as _, TryFutureExt},
};
use futures_ext::{BoxFuture, FutureExt};
use futures_old::future::Future;
use futures_old::{future, IntoFuture};
use mercurial_types::Globalrev;
use mononoke_types::{BonsaiChangeset, ChangesetId, RepositoryId};
use slog::warn;
use sql::queries;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BonsaiGlobalrevMappingEntry {
    pub repo_id: RepositoryId,
    pub bcs_id: ChangesetId,
    pub globalrev: Globalrev,
}

impl BonsaiGlobalrevMappingEntry {
    pub fn new(repo_id: RepositoryId, bcs_id: ChangesetId, globalrev: Globalrev) -> Self {
        BonsaiGlobalrevMappingEntry {
            repo_id,
            bcs_id,
            globalrev,
        }
    }
}

pub enum BonsaisOrGlobalrevs {
    Bonsai(Vec<ChangesetId>),
    Globalrev(Vec<Globalrev>),
}

impl BonsaisOrGlobalrevs {
    pub fn is_empty(&self) -> bool {
        match self {
            BonsaisOrGlobalrevs::Bonsai(v) => v.is_empty(),
            BonsaisOrGlobalrevs::Globalrev(v) => v.is_empty(),
        }
    }
}

impl From<ChangesetId> for BonsaisOrGlobalrevs {
    fn from(cs_id: ChangesetId) -> Self {
        BonsaisOrGlobalrevs::Bonsai(vec![cs_id])
    }
}

impl From<Vec<ChangesetId>> for BonsaisOrGlobalrevs {
    fn from(cs_ids: Vec<ChangesetId>) -> Self {
        BonsaisOrGlobalrevs::Bonsai(cs_ids)
    }
}

impl From<Globalrev> for BonsaisOrGlobalrevs {
    fn from(rev: Globalrev) -> Self {
        BonsaisOrGlobalrevs::Globalrev(vec![rev])
    }
}

impl From<Vec<Globalrev>> for BonsaisOrGlobalrevs {
    fn from(revs: Vec<Globalrev>) -> Self {
        BonsaisOrGlobalrevs::Globalrev(revs)
    }
}

pub trait BonsaiGlobalrevMapping: Send + Sync {
    fn bulk_import(&self, entries: &[BonsaiGlobalrevMappingEntry]) -> BoxFuture<(), Error>;

    fn get(
        &self,
        repo_id: RepositoryId,
        field: BonsaisOrGlobalrevs,
    ) -> BoxFuture<Vec<BonsaiGlobalrevMappingEntry>, Error>;

    fn get_globalrev_from_bonsai(
        &self,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> BoxFuture<Option<Globalrev>, Error>;

    fn get_bonsai_from_globalrev(
        &self,
        repo_id: RepositoryId,
        globalrev: Globalrev,
    ) -> BoxFuture<Option<ChangesetId>, Error>;

    /// Read the most recent Globalrev. This produces the freshest data possible, and is meant to
    /// be used for Globalrev assignment.
    fn get_max(&self, repo_id: RepositoryId) -> BoxFuture<Option<Globalrev>, Error>;
}

impl BonsaiGlobalrevMapping for Arc<dyn BonsaiGlobalrevMapping> {
    fn bulk_import(&self, entries: &[BonsaiGlobalrevMappingEntry]) -> BoxFuture<(), Error> {
        (**self).bulk_import(entries)
    }

    fn get(
        &self,
        repo_id: RepositoryId,
        field: BonsaisOrGlobalrevs,
    ) -> BoxFuture<Vec<BonsaiGlobalrevMappingEntry>, Error> {
        (**self).get(repo_id, field)
    }

    fn get_globalrev_from_bonsai(
        &self,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> BoxFuture<Option<Globalrev>, Error> {
        (**self).get_globalrev_from_bonsai(repo_id, cs_id)
    }

    fn get_bonsai_from_globalrev(
        &self,
        repo_id: RepositoryId,
        globalrev: Globalrev,
    ) -> BoxFuture<Option<ChangesetId>, Error> {
        (**self).get_bonsai_from_globalrev(repo_id, globalrev)
    }

    fn get_max(&self, repo_id: RepositoryId) -> BoxFuture<Option<Globalrev>, Error> {
        (**self).get_max(repo_id)
    }
}

queries! {
    write DangerouslyAddGlobalrevs(values: (
        repo_id: RepositoryId,
        bcs_id: ChangesetId,
        globalrev: Globalrev,
    )) {
        insert_or_ignore,
        "{insert_or_ignore} INTO bonsai_globalrev_mapping (repo_id, bcs_id, globalrev) VALUES {values}"
    }

    read SelectMappingByBonsai(
        repo_id: RepositoryId,
        >list bcs_id: ChangesetId
    ) -> (ChangesetId, Globalrev) {
        "SELECT bcs_id, globalrev
         FROM bonsai_globalrev_mapping
         WHERE repo_id = {repo_id} AND bcs_id in {bcs_id}"
    }

    read SelectMappingByGlobalrev(
        repo_id: RepositoryId,
        >list globalrev: Globalrev
    ) -> (ChangesetId, Globalrev) {
        "SELECT bcs_id, globalrev
         FROM bonsai_globalrev_mapping
         WHERE repo_id = {repo_id} AND globalrev in {globalrev}"
    }

    read SelectMaxEntry(repo_id: RepositoryId) -> (Globalrev,) {
        "
        SELECT globalrev
        FROM bonsai_globalrev_mapping
        WHERE repo_id = {}
        ORDER BY globalrev DESC
        LIMIT 1
        "
    }
}

#[derive(Clone)]
pub struct SqlBonsaiGlobalrevMapping {
    write_connection: Connection,
    read_connection: Connection,
    read_master_connection: Connection,
}

impl SqlConstruct for SqlBonsaiGlobalrevMapping {
    const LABEL: &'static str = "bonsai_globalrev_mapping";

    const CREATION_QUERY: &'static str =
        include_str!("../schemas/sqlite-bonsai-globalrev-mapping.sql");

    fn from_sql_connections(connections: SqlConnections) -> Self {
        Self {
            write_connection: connections.write_connection,
            read_connection: connections.read_connection,
            read_master_connection: connections.read_master_connection,
        }
    }
}

impl SqlConstructFromMetadataDatabaseConfig for SqlBonsaiGlobalrevMapping {}

impl BonsaiGlobalrevMapping for SqlBonsaiGlobalrevMapping {
    fn bulk_import(&self, entries: &[BonsaiGlobalrevMappingEntry]) -> BoxFuture<(), Error> {
        let entries: Vec<_> = entries
            .iter()
            .map(
                |BonsaiGlobalrevMappingEntry {
                     repo_id,
                     bcs_id,
                     globalrev,
                 }| (repo_id, bcs_id, globalrev),
            )
            .collect();

        DangerouslyAddGlobalrevs::query(&self.write_connection, &entries[..])
            .from_err()
            .map(|_| ())
            .boxify()
    }

    fn get(
        &self,
        repo_id: RepositoryId,
        objects: BonsaisOrGlobalrevs,
    ) -> BoxFuture<Vec<BonsaiGlobalrevMappingEntry>, Error> {
        cloned!(self.read_master_connection);

        select_mapping(&self.read_connection, repo_id, &objects)
            .and_then(move |mut mappings| {
                let left_to_fetch = filter_fetched_objects(objects, &mappings[..]);

                if left_to_fetch.is_empty() {
                    Ok(mappings).into_future().left_future()
                } else {
                    select_mapping(&read_master_connection, repo_id, &left_to_fetch)
                        .map(move |mut master_mappings| {
                            mappings.append(&mut master_mappings);
                            mappings
                        })
                        .right_future()
                }
            })
            .boxify()
    }

    fn get_globalrev_from_bonsai(
        &self,
        repo_id: RepositoryId,
        bcs_id: ChangesetId,
    ) -> BoxFuture<Option<Globalrev>, Error> {
        self.get(repo_id, BonsaisOrGlobalrevs::Bonsai(vec![bcs_id]))
            .map(|result| result.into_iter().next().map(|entry| entry.globalrev))
            .boxify()
    }

    fn get_bonsai_from_globalrev(
        &self,
        repo_id: RepositoryId,
        globalrev: Globalrev,
    ) -> BoxFuture<Option<ChangesetId>, Error> {
        self.get(repo_id, BonsaisOrGlobalrevs::Globalrev(vec![globalrev]))
            .map(|result| result.into_iter().next().map(|entry| entry.bcs_id))
            .boxify()
    }

    fn get_max(&self, repo_id: RepositoryId) -> BoxFuture<Option<Globalrev>, Error> {
        cloned!(self.read_master_connection);

        async move {
            let row = SelectMaxEntry::query(&read_master_connection, &repo_id)
                .compat()
                .await?
                .into_iter()
                .next();

            Ok(row.map(|r| r.0))
        }
        .boxed()
        .compat()
        .boxify()
    }
}

fn filter_fetched_objects(
    objects: BonsaisOrGlobalrevs,
    mappings: &[BonsaiGlobalrevMappingEntry],
) -> BonsaisOrGlobalrevs {
    match objects {
        BonsaisOrGlobalrevs::Bonsai(cs_ids) => {
            let bcs_fetched: HashSet<_> = mappings.iter().map(|m| &m.bcs_id).collect();

            BonsaisOrGlobalrevs::Bonsai(
                cs_ids
                    .iter()
                    .filter_map(|cs| {
                        if !bcs_fetched.contains(cs) {
                            Some(*cs)
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
        }
        BonsaisOrGlobalrevs::Globalrev(globalrevs) => {
            let globalrevs_fetched: HashSet<_> = mappings.iter().map(|m| &m.globalrev).collect();

            BonsaisOrGlobalrevs::Globalrev(
                globalrevs
                    .iter()
                    .filter_map(|globalrev| {
                        if !globalrevs_fetched.contains(globalrev) {
                            Some(*globalrev)
                        } else {
                            None
                        }
                    })
                    .collect(),
            )
        }
    }
}

fn select_mapping(
    connection: &Connection,
    repo_id: RepositoryId,
    objects: &BonsaisOrGlobalrevs,
) -> BoxFuture<Vec<BonsaiGlobalrevMappingEntry>, Error> {
    cloned!(repo_id, objects);
    if objects.is_empty() {
        return future::ok(vec![]).boxify();
    }

    let rows_fut = match objects {
        BonsaisOrGlobalrevs::Bonsai(bcs_ids) => {
            SelectMappingByBonsai::query(&connection, &repo_id, &bcs_ids[..]).left_future()
        }
        BonsaisOrGlobalrevs::Globalrev(globalrevs) => {
            SelectMappingByGlobalrev::query(&connection, &repo_id, &globalrevs[..]).right_future()
        }
    };

    rows_fut
        .map(move |rows| {
            rows.into_iter()
                .map(move |(bcs_id, globalrev)| BonsaiGlobalrevMappingEntry {
                    repo_id,
                    bcs_id,
                    globalrev,
                })
                .collect()
        })
        .boxify()
}

/// This method is for importing Globalrevs in bulk from a set of BonsaiChangesets where you know
/// they are correct. Don't use this to assign new Globalrevs.
pub fn bulk_import_globalrevs<'a>(
    ctx: CoreContext,
    repo_id: RepositoryId,
    globalrevs_store: Arc<dyn BonsaiGlobalrevMapping>,
    changesets: impl IntoIterator<Item = &'a BonsaiChangeset>,
) -> BoxFuture<(), Error> {
    let mut entries = vec![];
    for bcs in changesets.into_iter() {
        match Globalrev::from_bcs(bcs) {
            Ok(globalrev) => {
                let entry =
                    BonsaiGlobalrevMappingEntry::new(repo_id, bcs.get_changeset_id(), globalrev);
                entries.push(entry);
            }
            Err(e) => {
                warn!(
                    ctx.logger(),
                    "Couldn't fetch globalrev from commit: {:?}", e
                );
            }
        }
    }
    globalrevs_store.bulk_import(&entries)
}

#[derive(Debug, Error)]
pub enum AddGlobalrevsErrorKind {
    #[error("Conflict detected while inserting Globalrevs")]
    Conflict,

    #[error("Internal error occurred while inserting Globalrevs")]
    InternalError(#[source] Error),
}

impl From<Error> for AddGlobalrevsErrorKind {
    fn from(e: Error) -> Self {
        AddGlobalrevsErrorKind::InternalError(e)
    }
}

// NOTE: For now, this is a top-level function since it doesn't use the connections in the
// SqlBonsaiGlobalrevMapping, but if we were to add more implementations of the
// BonsaiGlobalrevMapping trait, we should probably rethink the design of it, and not actually have
// it contain any connections (instead, they should be passed on by callers).
pub async fn add_globalrevs(
    transaction: Transaction,
    entries: impl IntoIterator<Item = &BonsaiGlobalrevMappingEntry>,
) -> Result<Transaction, AddGlobalrevsErrorKind> {
    let rows: Vec<_> = entries
        .into_iter()
        .map(
            |BonsaiGlobalrevMappingEntry {
                 repo_id,
                 bcs_id,
                 globalrev,
             }| (repo_id, bcs_id, globalrev),
        )
        .collect();

    // It'd be really nice if we could rely on the error from an index conflict here, but our SQL
    // crate doesn't allow us to reach into this yet, so for now we check the number of affected
    // rows.

    let (transaction, res) =
        DangerouslyAddGlobalrevs::query_with_transaction(transaction, &rows[..])
            .compat()
            .await?;

    if res.affected_rows() != rows.len() as u64 {
        return Err(AddGlobalrevsErrorKind::Conflict);
    }

    Ok(transaction)
}
