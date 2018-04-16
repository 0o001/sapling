// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]
#![feature(try_from)]

extern crate db;
#[macro_use]
extern crate diesel;
#[macro_use]
extern crate failure_ext as failure;
extern crate futures;

extern crate filenodes;
#[macro_use]
extern crate futures_ext;
extern crate mercurial_types;
extern crate mononoke_types;

use db::ConnectionParams;
use diesel::{insert_or_ignore_into, Connection, SqliteConnection};
use diesel::backend::Backend;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::HasSqlType;
use failure::{Error, Result};
use filenodes::{FilenodeInfo, Filenodes};
use futures::{future, Future, Stream};
use futures_ext::{BoxFuture, BoxStream, FutureExt};
use mercurial_types::{DFileNodeId, RepoPath, RepositoryId};
use mercurial_types::sql_types::DFileNodeIdSql;

use std::sync::{Arc, Mutex};

mod common;
mod errors;
mod models;
mod schema;

use errors::ErrorKind;

pub const DEFAULT_INSERT_CHUNK_SIZE: usize = 100;

pub struct SqliteFilenodes {
    connection: Arc<Mutex<SqliteConnection>>,
    insert_chunk_size: usize,
}

impl SqliteFilenodes {
    /// Open a SQLite database. This is synchronous because the SQLite backend hits local
    /// disk or memory.
    pub fn open<P: AsRef<str>>(path: P, insert_chunk_size: usize) -> Result<Self> {
        let path = path.as_ref();
        let conn = SqliteConnection::establish(path)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(conn)),
            insert_chunk_size,
        })
    }

    fn create_tables(&mut self) -> Result<()> {
        let up_query = include_str!("../schemas/sqlite-filenodes.sql");

        self.connection
            .lock()
            .expect("lock poisoned")
            .batch_execute(&up_query)?;

        Ok(())
    }

    /// Create a new SQLite database.
    pub fn create<P: AsRef<str>>(path: P, insert_chunk_size: usize) -> Result<Self> {
        let mut changesets = Self::open(path, insert_chunk_size)?;

        changesets.create_tables()?;

        Ok(changesets)
    }

    /// Open a SQLite database, and create the tables if they are missing
    pub fn open_or_create<P: AsRef<str>>(path: P, insert_chunk_size: usize) -> Result<Self> {
        let mut filenodes = Self::open(path, insert_chunk_size)?;

        let _ = filenodes.create_tables();

        Ok(filenodes)
    }

    /// Create a new in-memory empty database. Great for tests.
    pub fn in_memory() -> Result<Self> {
        Self::create(":memory:", DEFAULT_INSERT_CHUNK_SIZE)
    }
}

pub struct MysqlFilenodes {
    connection: Arc<Mutex<MysqlConnection>>,
    insert_chunk_size: usize,
}

impl MysqlFilenodes {
    pub fn open(params: ConnectionParams, insert_chunk_size: usize) -> Result<Self> {
        let url = params.to_diesel_url()?;
        let conn = MysqlConnection::establish(&url)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(conn)),
            insert_chunk_size,
        })
    }

    pub fn create_test_db<P: AsRef<str>>(prefix: P) -> Result<Self> {
        let params = db::create_test_db(prefix)?;
        Self::create(params)
    }

    fn create(params: ConnectionParams) -> Result<Self> {
        let filenodes = Self::open(params, DEFAULT_INSERT_CHUNK_SIZE)?;

        let up_query = include_str!("../schemas/mysql-filenodes.sql");
        filenodes
            .connection
            .lock()
            .expect("lock poisoned")
            .batch_execute(&up_query)?;

        Ok(filenodes)
    }
}

macro_rules! impl_filenodes {
    ($struct: ty, $connection: ty) => {
        impl Filenodes for $struct {
            fn add_filenodes(
                &self,
                filenodes: BoxStream<FilenodeInfo, Error>,
                repo_id: &RepositoryId,
            ) -> BoxFuture<(), Error> {
                let repo_id = *repo_id;
                let connection = self.connection.clone();
                filenodes.chunks(self.insert_chunk_size).and_then(move |filenodes| {
                    let connection = connection.lock().expect("poisoned lock");
                    Self::do_insert(&connection, &filenodes, &repo_id)
                })
                .for_each(|()| Ok(()))
                .boxify()
            }

            fn get_filenode(
                &self,
                path: &RepoPath,
                filenode: &DFileNodeId,
                repo_id: &RepositoryId,
            ) -> BoxFuture<Option<FilenodeInfo>, Error> {
                let connection = self.connection.lock().expect("lock poisoned");

                let query = filenode_query(repo_id, filenode, path);
                let filenode_row = try_boxfuture!(
                    query.first::<models::FilenodeRow>(&*connection).optional());
                match filenode_row {
                    Some(filenode_row) => {
                        let copyfrom = try_boxfuture!(Self::fetch_copydata(
                            &*connection,
                            filenode,
                            path,
                            repo_id,
                        ));

                        let filenodeinfo = FilenodeInfo {
                            path: path.clone(),
                            filenode: filenode.clone(),
                            p1: filenode_row.p1,
                            p2: filenode_row.p2,
                            copyfrom,
                            linknode: filenode_row.linknode,
                        };
                        future::ok::<_, Error>(Some(filenodeinfo)).boxify()
                    }
                    None => future::ok::<_, Error>(None).boxify(),
                }
            }
        }

        impl $struct {
            fn do_insert(
                connection: &$connection,
                filenodes: &Vec<FilenodeInfo>,
                repo_id: &RepositoryId,
            ) -> BoxFuture<(), Error> {
                let txnres = connection.transaction::<_, Error, _>(|| {
                    Self::ensure_paths_exists(&*connection, repo_id, &filenodes)?;

                    Self::insert_filenodes(
                        &*connection,
                        &filenodes,
                        repo_id,
                    )?;
                    Ok(())
                });
                future::result(txnres).from_err().boxify()
            }

            fn ensure_paths_exists(
                connection: &$connection,
                repo_id: &RepositoryId,
                filenodes: &Vec<FilenodeInfo>,
            ) -> Result<()> {
                let mut path_rows = vec![];
                for filenode in filenodes {
                    let (path_bytes, _) = convert_from_repo_path(&filenode.path);
                    let path_row = models::PathRow::new(repo_id, path_bytes);
                    path_rows.push(path_row);
                }

                insert_or_ignore_into(schema::paths::table)
                    .values(&path_rows)
                    .execute(&*connection)?;
                Ok(())
            }

            fn insert_filenodes(
                connection: &$connection,
                filenodes: &Vec<FilenodeInfo>,
                repo_id: &RepositoryId,
            ) -> Result<()> {
                let mut filenode_rows = vec![];
                let mut copydata_rows = vec![];
                for filenode in filenodes.clone() {
                    let (path_bytes, is_tree) = convert_from_repo_path(&filenode.path);
                    let filenode_row = models::FilenodeRow::new(
                        repo_id,
                        &path_bytes,
                        is_tree,
                        &filenode.filenode,
                        &filenode.linknode,
                        filenode.p1,
                        filenode.p2,
                    );
                    filenode_rows.push(filenode_row);
                    if let Some(copyinfo) = filenode.copyfrom {
                        let (frompath, fromnode) = copyinfo;
                        let (frompath_bytes, from_is_tree) = convert_from_repo_path(&frompath);
                        if from_is_tree != is_tree {
                            return Err(ErrorKind::InvalidCopy(filenode.path, frompath).into());
                        }
                        let copyinfo_row = models::FixedCopyInfoRow::new(
                            repo_id,
                            &frompath_bytes,
                            &fromnode,
                            is_tree,
                            &path_bytes,
                            &filenode.filenode,
                        );
                        copydata_rows.push(copyinfo_row);
                    }
                }

                // Do not try to insert filenode again - even if linknode is different!
                // That matches core hg behavior.
                insert_or_ignore_into(schema::filenodes::table)
                    .values(&filenode_rows)
                    .execute(&*connection)?;

                insert_or_ignore_into(schema::fixedcopyinfo::table)
                    .values(&copydata_rows)
                    .execute(&*connection)?;
                Ok(())
            }

            fn fetch_copydata(
                connection: &$connection,
                filenode: &DFileNodeId,
                path: &RepoPath,
                repo_id: &RepositoryId,
            ) -> Result<Option<(RepoPath, DFileNodeId)>> {
                let is_tree = match path {
                    &RepoPath::RootPath | &RepoPath::DirectoryPath(_) => true,
                    &RepoPath::FilePath(_) => false,
                };

                let copyinfoquery = copyinfo_query(repo_id, filenode, path);

                let copydata_row =
                    copyinfoquery.first::<models::FixedCopyInfoRow>(&*connection)
                    .optional()?;
                if let Some(copydata) = copydata_row {
                    let path_row = path_query(repo_id, &copydata.frompath_hash)
                        .first::<models::PathRow>(&*connection)
                        .optional()?;
                    match path_row {
                        Some(path_row) => {
                            let frompath = convert_to_repo_path(&path_row.path, is_tree)?;
                            Ok(Some((frompath, copydata.fromnode)))
                        }
                        None => {
                            let err: Error = ErrorKind::PathNotFound(copydata.frompath_hash).into();
                            Err(err)
                        }
                    }
                } else {
                    Ok(None)
                }
            }
        }
    }
}

impl_filenodes!(MysqlFilenodes, MysqlConnection);
impl_filenodes!(SqliteFilenodes, SqliteConnection);

fn filenode_query<DB>(
    repo_id: &RepositoryId,
    filenode: &DFileNodeId,
    path: &RepoPath,
) -> schema::filenodes::BoxedQuery<'static, DB>
where
    DB: Backend,
    DB: HasSqlType<DFileNodeIdSql>,
{
    let (path_bytes, is_tree) = convert_from_repo_path(path);

    let path_hash = common::blake2_path_hash(&path_bytes);

    schema::filenodes::table
        .filter(schema::filenodes::repo_id.eq(*repo_id))
        .filter(schema::filenodes::filenode.eq(*filenode))
        .filter(schema::filenodes::path_hash.eq(path_hash.clone()))
        .filter(schema::filenodes::is_tree.eq(is_tree))
        .limit(1)
        .into_boxed()
}

fn copyinfo_query<DB>(
    repo_id: &RepositoryId,
    tonode: &DFileNodeId,
    topath: &RepoPath,
) -> schema::fixedcopyinfo::BoxedQuery<'static, DB>
where
    DB: Backend,
    DB: HasSqlType<DFileNodeIdSql>,
{
    let (topath_bytes, is_tree) = convert_from_repo_path(topath);

    let topath_hash = common::blake2_path_hash(&topath_bytes);

    schema::fixedcopyinfo::table
        .filter(schema::fixedcopyinfo::repo_id.eq(*repo_id))
        .filter(schema::fixedcopyinfo::topath_hash.eq(topath_hash))
        .filter(schema::fixedcopyinfo::tonode.eq(*tonode))
        .filter(schema::fixedcopyinfo::is_tree.eq(is_tree))
        .limit(1)
        .into_boxed()
}

fn path_query<DB>(
    repo_id: &RepositoryId,
    path_hash: &Vec<u8>,
) -> schema::paths::BoxedQuery<'static, DB>
where
    DB: Backend,
    DB: HasSqlType<DFileNodeIdSql>,
{
    schema::paths::table
        .filter(schema::paths::repo_id.eq(*repo_id))
        .filter(schema::paths::path_hash.eq(path_hash.clone()))
        .limit(1)
        .into_boxed()
}

fn convert_from_repo_path(path: &RepoPath) -> (Vec<u8>, i32) {
    match path {
        &RepoPath::RootPath => (vec![], 1),
        &RepoPath::DirectoryPath(ref dir) => (dir.to_vec(), 1),
        &RepoPath::FilePath(ref file) => (file.to_vec(), 0),
    }
}

fn convert_to_repo_path<B: AsRef<[u8]>>(path_bytes: B, is_tree: bool) -> Result<RepoPath> {
    if is_tree {
        RepoPath::dir(path_bytes.as_ref())
    } else {
        RepoPath::file(path_bytes.as_ref())
    }
}
