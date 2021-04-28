/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use fbinit::FacebookInit;
use metaconfig_types::{
    DatabaseConfig, LocalDatabaseConfig, MetadataDatabaseConfig, RemoteDatabaseConfig,
    RemoteMetadataDatabaseConfig, ShardableRemoteDatabaseConfig,
};
use sql_ext::facebook::MysqlOptions;

use crate::construct::SqlConstruct;
use crate::facebook::{FbSqlConstruct, FbSqlShardedConstruct};

/// Trait that allows construction from database config.
#[async_trait]
pub trait SqlConstructFromDatabaseConfig: FbSqlConstruct + SqlConstruct {
    async fn with_database_config(
        fb: FacebookInit,
        database_config: &DatabaseConfig,
        mysql_options: &MysqlOptions,
        readonly: bool,
    ) -> Result<Self> {
        match database_config {
            DatabaseConfig::Local(LocalDatabaseConfig { path }) => {
                Self::with_sqlite_path(path.join("sqlite_dbs"), readonly)
            }
            DatabaseConfig::Remote(config) => {
                Self::with_mysql(fb, config.db_address.clone(), mysql_options, readonly)
            }
        }
        .with_context(|| {
            format!(
                "While connecting to {:?} (with options {:?})",
                database_config, mysql_options
            )
        })
    }
}

impl<T: SqlConstruct + FbSqlConstruct> SqlConstructFromDatabaseConfig for T {}

/// Trait that allows construction from the metadata database config.
#[async_trait]
pub trait SqlConstructFromMetadataDatabaseConfig: FbSqlConstruct + SqlConstruct {
    async fn with_metadata_database_config(
        fb: FacebookInit,
        metadata_database_config: &MetadataDatabaseConfig,
        mysql_options: &MysqlOptions,
        readonly: bool,
    ) -> Result<Self> {
        match metadata_database_config {
            MetadataDatabaseConfig::Local(LocalDatabaseConfig { path }) => {
                Self::with_sqlite_path(path.join("sqlite_dbs"), readonly)
            }
            MetadataDatabaseConfig::Remote(remote) => {
                let config = Self::remote_database_config(remote)
                    .ok_or_else(|| anyhow!("no configuration available"))?;
                Self::with_mysql(fb, config.db_address.clone(), mysql_options, readonly)
            }
        }
    }

    /// Get the remote database config for this type.  Override this to use a database other than
    /// the primary database.
    fn remote_database_config(
        remote: &RemoteMetadataDatabaseConfig,
    ) -> Option<&RemoteDatabaseConfig> {
        Some(&remote.primary)
    }
}

/// Trait that allows construction of shardable databases from the metadata database config.
#[async_trait]
pub trait SqlShardableConstructFromMetadataDatabaseConfig:
    FbSqlConstruct + FbSqlShardedConstruct + SqlConstruct
{
    async fn with_metadata_database_config(
        fb: FacebookInit,
        metadata_database_config: &MetadataDatabaseConfig,
        mysql_options: &MysqlOptions,
        readonly: bool,
    ) -> Result<Self> {
        match metadata_database_config {
            MetadataDatabaseConfig::Local(LocalDatabaseConfig { path }) => {
                Self::with_sqlite_path(path.join("sqlite_dbs"), readonly)
            }
            MetadataDatabaseConfig::Remote(remote) => {
                let config = Self::remote_database_config(remote)
                    .ok_or_else(|| anyhow!("no configuration available"))?;
                match config {
                    ShardableRemoteDatabaseConfig::Unsharded(config) => {
                        Self::with_mysql(fb, config.db_address.clone(), mysql_options, readonly)
                    }
                    ShardableRemoteDatabaseConfig::Sharded(config) => Self::with_sharded_mysql(
                        fb,
                        config.shard_map.clone(),
                        config.shard_num.get(),
                        mysql_options,
                        readonly,
                    ),
                }
            }
        }
    }

    /// Get the remote database config for this type.
    fn remote_database_config(
        remote: &RemoteMetadataDatabaseConfig,
    ) -> Option<&ShardableRemoteDatabaseConfig>;
}
