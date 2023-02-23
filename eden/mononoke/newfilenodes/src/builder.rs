/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use cachelib::VolatileLruCachePool;
use fbinit::FacebookInit;
use metaconfig_types::RemoteMetadataDatabaseConfig;
use metaconfig_types::ShardableRemoteDatabaseConfig;
use mononoke_types::RepositoryId;
use sql::Connection;
use sql_construct::SqlShardableConstructFromMetadataDatabaseConfig;
use sql_construct::SqlShardedConstruct;
use sql_ext::SqlShardedConnections;

use crate::local_cache::LocalCache;
use crate::reader::FilenodesReader;
use crate::remote_cache::MemcacheCache;
use crate::remote_cache::RemoteCache;
use crate::writer::FilenodesWriter;
use crate::NewFilenodes;

pub const MYSQL_INSERT_CHUNK_SIZE: usize = 1000;
pub const SQLITE_INSERT_CHUNK_SIZE: usize = 100;

pub struct NewFilenodesBuilder {
    reader: FilenodesReader,
    writer: FilenodesWriter,
}

impl SqlShardedConstruct for NewFilenodesBuilder {
    const LABEL: &'static str = "shardedfilenodes";

    const CREATION_QUERY: &'static str = include_str!("../schemas/sqlite-filenodes.sql");

    fn from_sql_shard_connections(shard_connections: SqlShardedConnections) -> Self {
        let SqlShardedConnections {
            read_connections,
            read_master_connections,
            write_connections,
        } = shard_connections;
        let chunk_size = match read_connections.get(0) {
            Some(Connection::Mysql(_)) => MYSQL_INSERT_CHUNK_SIZE,
            _ => SQLITE_INSERT_CHUNK_SIZE,
        };

        let reader = FilenodesReader::new(read_connections.clone(), read_master_connections);
        let writer = FilenodesWriter::new(chunk_size, write_connections, read_connections);

        Self { reader, writer }
    }
}

impl SqlShardableConstructFromMetadataDatabaseConfig for NewFilenodesBuilder {
    fn remote_database_config(
        remote: &RemoteMetadataDatabaseConfig,
    ) -> Option<&ShardableRemoteDatabaseConfig> {
        Some(&remote.filenodes)
    }
}

impl NewFilenodesBuilder {
    pub fn build(self, repo_id: RepositoryId) -> NewFilenodes {
        NewFilenodes {
            reader: Arc::new(self.reader),
            writer: Arc::new(self.writer),
            repo_id,
        }
    }

    pub fn enable_caching(
        &mut self,
        fb: FacebookInit,
        filenodes_cache_pool: VolatileLruCachePool,
        filenodes_history_cache_pool: VolatileLruCachePool,
        backing_store_name: &str,
        backing_store_params: &str,
    ) {
        self.reader.local_cache = LocalCache::new(
            filenodes_cache_pool.into(),
            filenodes_history_cache_pool.into(),
        );

        self.reader.remote_cache = RemoteCache::Memcache(MemcacheCache::new(
            fb,
            backing_store_name,
            backing_store_params,
        ));
    }
}
