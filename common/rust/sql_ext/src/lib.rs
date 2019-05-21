// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

extern crate failure_ext as failure;
extern crate sql;

use std::path::Path;

use failure::prelude::*;
use sql::{myrouter, Connection, rusqlite::Connection as SqliteConnection};

pub struct SqlConnections {
    pub write_connection: Connection,
    pub read_connection: Connection,
    pub read_master_connection: Connection,
}

pub struct PoolSizeConfig {
    pub write_pool_size: usize,
    pub read_pool_size: usize,
    pub read_master_pool_size: usize,
}

impl PoolSizeConfig {
    fn for_regular_connection() -> Self {
        Self {
            write_pool_size: 1,
            read_pool_size: myrouter::DEFAULT_MAX_NUM_OF_CONCURRENT_CONNECTIONS,
            // For reading from master we need to use less concurrent connections in order to
            // protect the master from being overloaded. The `clone` here means that for write
            // connection we still use the default number of concurrent connections.
            read_master_pool_size: 10,
        }
    }

    pub fn for_sharded_connection() -> Self {
        Self {
            write_pool_size: 1,
            read_pool_size: 1,
            read_master_pool_size: 1,
        }
    }
}

pub fn create_myrouter_connections(
    tier: impl ToString,
    port: u16,
    pool_size_config: PoolSizeConfig,
) -> SqlConnections {
    let mut builder = Connection::myrouter_builder();
    builder.tier(tier).port(port);

    builder.tie_break(myrouter::TieBreak::SLAVE_FIRST);
    let read_connection = builder
        .max_num_of_concurrent_connections(pool_size_config.read_pool_size)
        .build_read_only();

    builder.service_type(myrouter::ServiceType::MASTER);
    let read_master_connection = builder
        .clone()
        .max_num_of_concurrent_connections(pool_size_config.read_master_pool_size)
        .build_read_only();

    let write_connection = builder
        .max_num_of_concurrent_connections(pool_size_config.write_pool_size)
        .build_read_write();

    SqlConnections {
        write_connection,
        read_connection,
        read_master_connection,
    }
}

/// Set of useful constructors for Mononoke's sql based data access objects
pub trait SqlConstructors: Sized {
    const LABEL: &'static str;

    fn from_connections(
        write_connection: Connection,
        read_connection: Connection,
        read_master_connection: Connection,
    ) -> Self;

    fn get_up_query() -> &'static str;

    fn with_myrouter(tier: impl ToString, port: u16) -> Self {
        let SqlConnections {
            write_connection,
            read_connection,
            read_master_connection,
        } = create_myrouter_connections(tier, port, PoolSizeConfig::for_regular_connection());

        Self::from_connections(write_connection, read_connection, read_master_connection)
    }

    fn with_sqlite_in_memory() -> Result<Self> {
        let con = SqliteConnection::open_in_memory()?;
        con.execute_batch(Self::get_up_query())?;
        with_sqlite(con)
    }

    fn with_sqlite_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let con = SqliteConnection::open(path)?;
        // When opening an sqlite database we might already have the proper tables in it, so ignore
        // errors from table creation
        let _ = con.execute_batch(Self::get_up_query());
        with_sqlite(con)
    }
}

fn with_sqlite<T: SqlConstructors>(con: SqliteConnection) -> Result<T> {
    let con = Connection::with_sqlite(con);
    Ok(T::from_connections(con.clone(), con.clone(), con))
}
