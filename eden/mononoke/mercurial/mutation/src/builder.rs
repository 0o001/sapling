/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use metaconfig_types::{RemoteDatabaseConfig, RemoteMetadataDatabaseConfig};
use mononoke_types::RepositoryId;
use sql_construct::{SqlConstruct, SqlConstructFromMetadataDatabaseConfig};
use sql_ext::SqlConnections;

use crate::store::SqlHgMutationStore;

#[allow(unused)]
pub struct SqlHgMutationStoreBuilder {
    pub(crate) connections: SqlConnections,
}

impl SqlConstruct for SqlHgMutationStoreBuilder {
    const LABEL: &'static str = "hg_mutations";

    const CREATION_QUERY: &'static str = include_str!("../schemas/sqlite-hg-mutations.sql");

    fn from_sql_connections(connections: SqlConnections) -> Self {
        Self { connections }
    }
}

impl SqlConstructFromMetadataDatabaseConfig for SqlHgMutationStoreBuilder {
    fn remote_database_config(
        remote: &RemoteMetadataDatabaseConfig,
    ) -> Option<&RemoteDatabaseConfig> {
        Some(&remote.mutation)
    }
}

impl SqlHgMutationStoreBuilder {
    pub fn with_repo_id(self, repo_id: RepositoryId) -> SqlHgMutationStore {
        SqlHgMutationStore::new(repo_id, self.connections)
    }
}
