/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::base::{ErrorKind, MultiplexedBlobstoreBase, MultiplexedBlobstorePutHandler};
use anyhow::Result;
use async_trait::async_trait;
use blobstore::{Blobstore, BlobstoreGetData, BlobstorePutOps, OverwriteStatus, PutBehaviour};
use blobstore_sync_queue::{BlobstoreSyncQueue, BlobstoreSyncQueueEntry, OperationKey};
use context::CoreContext;
use metaconfig_types::{BlobstoreId, MultiplexId};
use mononoke_types::{BlobstoreBytes, DateTime};
use scuba_ext::MononokeScubaSampleBuilder;
use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;

#[derive(Clone)]
pub struct MultiplexedBlobstore {
    pub(crate) blobstore: Arc<MultiplexedBlobstoreBase>,
    queue: Arc<dyn BlobstoreSyncQueue>,
}

impl MultiplexedBlobstore {
    pub fn new(
        multiplex_id: MultiplexId,
        blobstores: Vec<(BlobstoreId, Arc<dyn BlobstorePutOps>)>,
        write_mostly_blobstores: Vec<(BlobstoreId, Arc<dyn BlobstorePutOps>)>,
        minimum_successful_writes: NonZeroUsize,
        queue: Arc<dyn BlobstoreSyncQueue>,
        scuba: MononokeScubaSampleBuilder,
        scuba_sample_rate: NonZeroU64,
    ) -> Self {
        let put_handler = Arc::new(QueueBlobstorePutHandler {
            queue: queue.clone(),
        });
        Self {
            blobstore: Arc::new(MultiplexedBlobstoreBase::new(
                multiplex_id,
                blobstores,
                write_mostly_blobstores,
                minimum_successful_writes,
                put_handler,
                scuba,
                scuba_sample_rate,
            )),
            queue,
        }
    }
}

impl fmt::Display for MultiplexedBlobstore {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MultiplexedBlobstore[{}]", self.blobstore.as_ref())
    }
}

impl fmt::Debug for MultiplexedBlobstore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MultiplexedBlobstore")
            .field("base", &self.blobstore)
            .finish()
    }
}

struct QueueBlobstorePutHandler {
    queue: Arc<dyn BlobstoreSyncQueue>,
}

#[async_trait]
impl MultiplexedBlobstorePutHandler for QueueBlobstorePutHandler {
    async fn on_put<'out>(
        &'out self,
        ctx: &'out CoreContext,
        blobstore_id: BlobstoreId,
        multiplex_id: MultiplexId,
        operation_key: &'out OperationKey,
        key: &'out str,
        blob_size: Option<u64>,
    ) -> Result<()> {
        self.queue
            .add(
                ctx,
                BlobstoreSyncQueueEntry::new(
                    key.to_string(),
                    blobstore_id,
                    multiplex_id,
                    DateTime::now(),
                    operation_key.clone(),
                    blob_size,
                ),
            )
            .await
    }
}

#[async_trait]
impl Blobstore for MultiplexedBlobstore {
    async fn get<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<Option<BlobstoreGetData>> {
        let result = self.blobstore.get(ctx, key).await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some(ErrorKind::AllFailed(_)) = error.downcast_ref() {
                    return Err(error);
                }
                // This means that some underlying blobstore returned error, and
                // other return None. To distinguish incomplete sync from true-none we
                // check synchronization queue. If it does not contain entries with this key
                // it means it is true-none otherwise, only replica containing key has
                // failed and we need to return error.
                let entries = self.queue.get(ctx, key).await?;
                if entries.is_empty() {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
        }
    }

    async fn put<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<()> {
        self.blobstore.put(ctx, key, value).await
    }

    async fn is_present<'a>(&'a self, ctx: &'a CoreContext, key: &'a str) -> Result<bool> {
        let result = self.blobstore.is_present(ctx, key).await;
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Some(ErrorKind::AllFailed(_)) = error.downcast_ref() {
                    return Err(error);
                }
                // If a subset of blobstores failed, then we go to the queue. This is a way to
                // "break the tie" if we had at least one blobstore that said the content didn't
                // exist but the others failed to give a response: if any of those failing
                // blobstores has the content, then it *must* be on the queue (it cannot have been
                // pruned yet because if it was, then it would be in the blobstore that succeeded).
                let entries = self.queue.get(&ctx, &key).await?;
                if entries.is_empty() {
                    Ok(false)
                } else {
                    // Oh boy. If we found this on the queue but we didn't find it in the
                    // blobstores, it's possible that the content got written to the blobstore in
                    // the meantime. To account for this ... we have to check again.
                    self.blobstore.is_present(ctx, key).await
                }
            }
        }
    }
}

#[async_trait]
impl BlobstorePutOps for MultiplexedBlobstore {
    async fn put_explicit<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
        put_behaviour: PutBehaviour,
    ) -> Result<OverwriteStatus> {
        self.blobstore
            .put_explicit(ctx, key, value, put_behaviour)
            .await
    }

    async fn put_with_status<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<OverwriteStatus> {
        self.blobstore.put_with_status(ctx, key, value).await
    }
}
