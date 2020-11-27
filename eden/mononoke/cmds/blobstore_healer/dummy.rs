/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! This dummy crate contains dummy implementation of traits that are being used only in the
//! --dry-run mode to test the healer

use anyhow::Result;
use async_trait::async_trait;
use blobstore::{Blobstore, BlobstoreGetData};
use blobstore_sync_queue::{BlobstoreSyncQueue, BlobstoreSyncQueueEntry};
use context::CoreContext;
use metaconfig_types::MultiplexId;
use mononoke_types::{BlobstoreBytes, DateTime};
use slog::{info, Logger};

#[derive(Debug)]
pub struct DummyBlobstore<B> {
    inner: B,
    logger: Logger,
}

impl<'a, B: Blobstore> DummyBlobstore<B> {
    pub fn new(inner: B, logger: Logger) -> Self {
        Self { inner, logger }
    }
}

#[async_trait]
impl<B: Blobstore> Blobstore for DummyBlobstore<B> {
    async fn get<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<Option<BlobstoreGetData>> {
        self.inner.get(ctx, key).await
    }

    async fn put<'a>(
        &'a self,
        _ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<()> {
        info!(
            self.logger,
            "I would have written blob {} of size {}",
            key,
            value.len()
        );
        Ok(())
    }

    async fn is_present<'a>(&'a self, ctx: &'a CoreContext, key: &'a str) -> Result<bool> {
        self.inner.is_present(ctx, key).await
    }
}

pub struct DummyBlobstoreSyncQueue<Q> {
    inner: Q,
    logger: Logger,
}

impl<Q: BlobstoreSyncQueue> DummyBlobstoreSyncQueue<Q> {
    pub fn new(inner: Q, logger: Logger) -> Self {
        Self { inner, logger }
    }
}

#[async_trait]
impl<Q: BlobstoreSyncQueue> BlobstoreSyncQueue for DummyBlobstoreSyncQueue<Q> {
    async fn add_many<'a>(
        &'a self,
        _ctx: &'a CoreContext,
        entries: Vec<BlobstoreSyncQueueEntry>,
    ) -> Result<()> {
        let entries: Vec<_> = entries.into_iter().map(|e| format!("{:?}", e)).collect();
        info!(self.logger, "I would have written {}", entries.join(",\n"));
        Ok(())
    }

    async fn iter<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key_like: Option<&'a str>,
        multiplex_id: MultiplexId,
        older_than: DateTime,
        limit: usize,
    ) -> Result<Vec<BlobstoreSyncQueueEntry>> {
        self.inner
            .iter(ctx, key_like, multiplex_id, older_than, limit)
            .await
    }

    async fn del<'a>(
        &'a self,
        _ctx: &'a CoreContext,
        entries: &'a [BlobstoreSyncQueueEntry],
    ) -> Result<()> {
        let entries: Vec<_> = entries.iter().map(|e| format!("{:?}", e)).collect();
        info!(self.logger, "I would have deleted {}", entries.join(",\n"));
        Ok(())
    }

    async fn get<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<Vec<BlobstoreSyncQueueEntry>> {
        self.inner.get(ctx, key).await
    }
}
