// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]

#[macro_use]
extern crate sql;

use std::vec::Vec;

use bytes::Bytes;
use failure::{Error, Fail};
use futures::Future;
use futures_ext::{BoxFuture, FutureExt};
use sql::Connection;

use blobstore::Blobstore;
use context::CoreContext;
use mononoke_types::{BlobstoreBytes, RepositoryId};

#[derive(Debug, Fail)]
pub enum ErrorKind {
    #[fail(display = "internal error: streaming blob {} missing", _0)]
    MissingStreamingBlob(String),
}

pub struct RevlogStreamingChunks {
    pub index_size: usize,
    pub data_size: usize,
    pub index_blobs: Vec<BoxFuture<Bytes, Error>>,
    pub data_blobs: Vec<BoxFuture<Bytes, Error>>,
}

impl RevlogStreamingChunks {
    pub fn new() -> Self {
        Self {
            data_size: 0,
            index_size: 0,
            data_blobs: Vec::new(),
            index_blobs: Vec::new(),
        }
    }
}

#[derive(Clone)]
pub struct SqlStreamingChunksFetcher {
    read_connection: Connection,
}

queries! {
    read SelectChunks(repo_id: RepositoryId) -> (Vec<u8>, i32, Vec<u8>, i32) {
        "SELECT idx_blob_name, idx_size, data_blob_name, data_size
         FROM streaming_changelog_chunks
         WHERE repo_id = {repo_id}
         ORDER BY chunk_num ASC"
    }
}

impl SqlStreamingChunksFetcher {
    pub fn with_myrouter(tier: impl ToString, port: u16) -> Self {
        let mut builder = Connection::myrouter_builder();
        builder.tier(tier).port(port);

        let read_connection = builder.build_read_only();

        Self { read_connection }
    }

    pub fn fetch_changelog(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        blobstore: impl Blobstore + Clone,
    ) -> BoxFuture<RevlogStreamingChunks, Error> {
        SelectChunks::query(&self.read_connection, &repo_id)
            .map(move |rows| {
                rows.into_iter().fold(
                    RevlogStreamingChunks::new(),
                    move |mut res, (idx_blob_name, idx_size, data_blob_name, data_size)| {
                        res.data_size += data_size as usize;
                        res.index_size += idx_size as usize;
                        let data_blob_key = String::from_utf8_lossy(&data_blob_name).into_owned();
                        res.data_blobs.push(
                            blobstore
                                .get(ctx.clone(), data_blob_key.clone())
                                .and_then(|data| {
                                    data.ok_or(
                                        ErrorKind::MissingStreamingBlob(data_blob_key).into(),
                                    )
                                })
                                .map(BlobstoreBytes::into_bytes)
                                .boxify(),
                        );
                        let idx_blob_key = String::from_utf8_lossy(&idx_blob_name).into_owned();
                        res.index_blobs.push(
                            blobstore
                                .get(ctx.clone(), idx_blob_key.clone())
                                .and_then(|data| {
                                    data.ok_or(ErrorKind::MissingStreamingBlob(idx_blob_key).into())
                                })
                                .map(BlobstoreBytes::into_bytes)
                                .boxify(),
                        );
                        res
                    },
                )
            })
            .boxify()
    }
}
