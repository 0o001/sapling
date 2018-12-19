// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]

extern crate failure_ext as failure;
extern crate futures;

extern crate cloned;
extern crate context;
extern crate futures_ext;
extern crate metaconfig;
extern crate mononoke_types;
#[macro_use]
extern crate sql;
extern crate sql_ext;
#[macro_use]
extern crate stats;

use std::sync::Arc;

use context::CoreContext;
use sql::Connection;
pub use sql_ext::SqlConstructors;

use cloned::cloned;
use failure::{format_err, Error};
use futures::{future, Future, IntoFuture};
use futures_ext::{BoxFuture, FutureExt};
use metaconfig::BlobstoreId;
use mononoke_types::{DateTime, RepositoryId, Timestamp};

use stats::Timeseries;

define_stats! {
    prefix = "mononoke.blobstore_sync_queue";
    adds: timeseries(RATE, SUM),
    iters: timeseries(RATE, SUM),
    dels: timeseries(RATE, SUM),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BlobstoreSyncQueueEntry {
    pub repo_id: RepositoryId,
    pub blobstore_key: String,
    pub blobstore_id: BlobstoreId,
    pub timestamp: DateTime,
    pub id: Option<u64>,
}

impl BlobstoreSyncQueueEntry {
    pub fn new(
        repo_id: RepositoryId,
        blobstore_key: String,
        blobstore_id: BlobstoreId,
        timestamp: DateTime,
    ) -> Self {
        Self {
            repo_id,
            blobstore_key,
            blobstore_id,
            timestamp,
            id: None,
        }
    }
}

pub trait BlobstoreSyncQueue: Send + Sync {
    fn add(&self, ctx: CoreContext, entry: BlobstoreSyncQueueEntry) -> BoxFuture<bool, Error>;

    fn iter(
        &self,
        ctx: CoreContext,
        older_than: DateTime,
        limit: usize,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error>;

    fn del(&self, ctx: CoreContext, entries: Vec<BlobstoreSyncQueueEntry>) -> BoxFuture<(), Error>;

    fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        key: String,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error>;
}

impl BlobstoreSyncQueue for Arc<BlobstoreSyncQueue> {
    fn add(&self, ctx: CoreContext, entry: BlobstoreSyncQueueEntry) -> BoxFuture<bool, Error> {
        (**self).add(ctx, entry)
    }

    fn iter(
        &self,
        ctx: CoreContext,
        older_than: DateTime,
        limit: usize,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error> {
        (**self).iter(ctx, older_than, limit)
    }

    fn del(&self, ctx: CoreContext, entries: Vec<BlobstoreSyncQueueEntry>) -> BoxFuture<(), Error> {
        (**self).del(ctx, entries)
    }

    fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        key: String,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error> {
        (**self).get(ctx, repo_id, key)
    }
}

#[derive(Clone)]
pub struct SqlBlobstoreSyncQueue {
    write_connection: Connection,
    read_connection: Connection,
    read_master_connection: Connection,
}

queries! {
    write InsertEntry(values: (
        repo_id: RepositoryId,
        blobstore_key: String,
        blobstore_id: BlobstoreId,
        timestamp: Timestamp,
    )) {
        insert_or_ignore,
        "{insert_or_ignore}
         INTO blobstore_sync_queue (repo_id, blobstore_key, blobstore_id, add_timestamp)
         VALUES {values}"
    }

    write DeleteEntry(id: u64) {
        none,
        "DELETE FROM blobstore_sync_queue
         WHERE id = {id}"
    }

    read GetAllIEntries() -> (RepositoryId, String, BlobstoreId, Timestamp, u64) {
        "SELECT repo_id, blobstore_key, blobstore_id, add_timestamp, id
         FROM blobstore_sync_queue"
    }

    read GetRangeOfEntries(older_than: Timestamp, limit: usize) -> (
        RepositoryId,
        String,
        BlobstoreId,
        Timestamp,
        u64,
    ) {
        "SELECT repo_id, blobstore_key, blobstore_id, add_timestamp, id
         FROM blobstore_sync_queue
         WHERE add_timestamp >= {older_than}
         ORDER BY id
         LIMIT {limit}"
    }

    read GetByKey(repo_id: RepositoryId, key: String) -> (
        RepositoryId,
        String,
        BlobstoreId,
        Timestamp,
        u64,
    ) {
        "SELECT repo_id, blobstore_key, blobstore_id, add_timestamp, id
         FROM blobstore_sync_queue
         WHERE repo_id = {repo_id}
         AND blobstore_key = {key}"
    }
}

impl SqlConstructors for SqlBlobstoreSyncQueue {
    fn from_connections(
        write_connection: Connection,
        read_connection: Connection,
        read_master_connection: Connection,
    ) -> Self {
        Self {
            write_connection,
            read_connection,
            read_master_connection,
        }
    }

    fn get_up_query() -> &'static str {
        include_str!("../schemas/sqlite-blobstore-sync-queue.sql")
    }
}

impl BlobstoreSyncQueue for SqlBlobstoreSyncQueue {
    fn add(&self, _ctx: CoreContext, entry: BlobstoreSyncQueueEntry) -> BoxFuture<bool, Error> {
        STATS::adds.add_value(1);

        let BlobstoreSyncQueueEntry {
            repo_id,
            blobstore_key,
            blobstore_id,
            timestamp,
            ..
        } = entry.clone();

        InsertEntry::query(
            &self.write_connection,
            &[(&repo_id, &blobstore_key, &blobstore_id, &timestamp.into())],
        ).map(|result| result.affected_rows() == 1)
            .boxify()
    }

    fn iter(
        &self,
        _ctx: CoreContext,
        older_than: DateTime,
        limit: usize,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error> {
        STATS::iters.add_value(1);
        // query
        GetRangeOfEntries::query(&self.read_master_connection, &older_than.into(), &limit)
            .map(|rows| {
                rows.into_iter()
                    .map(|(repo_id, blobstore_key, blobstore_id, timestamp, id)| {
                        BlobstoreSyncQueueEntry {
                            repo_id,
                            blobstore_key,
                            blobstore_id,
                            timestamp: timestamp.into(),
                            id: Some(id),
                        }
                    })
                    .collect()
            })
            .boxify()
    }

    fn del(
        &self,
        _ctx: CoreContext,
        entries: Vec<BlobstoreSyncQueueEntry>,
    ) -> BoxFuture<(), Error> {
        STATS::dels.add_value(1);

        let ids: Result<Vec<u64>, Error> = entries
            .into_iter()
            .map(|entry| {
                entry.id.ok_or_else(|| {
                    format_err!("BlobstoreSyncQueueEntry must contain `id` to be able to delete it")
                })
            })
            .collect();
        ids.into_future()
            .and_then({
                cloned!(self.write_connection);
                move |ids| {
                    future::join_all(ids.into_iter().map({
                        cloned!(write_connection);
                        move |id| DeleteEntry::query(&write_connection, &id)
                    }))
                }
            })
            .map(|_| ())
            .boxify()
    }

    fn get(
        &self,
        _ctx: CoreContext,
        repo_id: RepositoryId,
        key: String,
    ) -> BoxFuture<Vec<BlobstoreSyncQueueEntry>, Error> {
        GetByKey::query(&self.read_master_connection, &repo_id, &key)
            .map(|rows| {
                rows.into_iter()
                    .map(|(repo_id, blobstore_key, blobstore_id, timestamp, id)| {
                        BlobstoreSyncQueueEntry {
                            repo_id,
                            blobstore_key,
                            blobstore_id,
                            timestamp: timestamp.into(),
                            id: Some(id),
                        }
                    })
                    .collect()
            })
            .boxify()
    }
}
