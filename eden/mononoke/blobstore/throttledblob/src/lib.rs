/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use anyhow::Result;
use async_trait::async_trait;
use std::fmt;
use std::num::NonZeroU32;

use async_limiter::AsyncLimiter;
use futures::future::{BoxFuture, FutureExt};
use ratelimit_meter::{algorithms::LeakyBucket, example_algorithms::Allower, DirectRateLimiter};

use blobstore::{Blobstore, BlobstoreGetData, BlobstorePutOps, OverwriteStatus, PutBehaviour};
use context::CoreContext;
use mononoke_types::BlobstoreBytes;

#[derive(Clone, Copy, Debug)]
pub struct ThrottleOptions {
    read_qps: Option<NonZeroU32>,
    write_qps: Option<NonZeroU32>,
}

impl ThrottleOptions {
    pub fn new(read_qps: Option<NonZeroU32>, write_qps: Option<NonZeroU32>) -> Self {
        Self {
            read_qps,
            write_qps,
        }
    }

    pub fn has_throttle(&self) -> bool {
        self.read_qps.is_some() || self.write_qps.is_some()
    }
}

/// A Blobstore that rate limits the number of read and write operations.
#[derive(Clone)]
pub struct ThrottledBlob<T: Clone + fmt::Debug> {
    blobstore: T,
    read_limiter: AsyncLimiter,
    write_limiter: AsyncLimiter,
    /// The options fields are used for Debug. They are not consulted at runtime.
    options: ThrottleOptions,
}

async fn limiter(qps: Option<NonZeroU32>) -> AsyncLimiter {
    match qps {
        Some(qps) => AsyncLimiter::new(DirectRateLimiter::<LeakyBucket>::per_second(qps)).await,
        None => AsyncLimiter::new(Allower::ratelimiter()).await,
    }
}

impl<T: Clone + fmt::Debug + Send + Sync> ThrottledBlob<T> {
    pub async fn new(blobstore: T, options: ThrottleOptions) -> Self {
        Self {
            blobstore,
            read_limiter: limiter(options.read_qps).await,
            write_limiter: limiter(options.write_qps).await,
            options,
        }
    }

    fn throttled_access<'a, ThrottledFn, Out>(
        &self,
        limiter: &AsyncLimiter,
        throttled_fn: ThrottledFn,
    ) -> BoxFuture<'a, Result<Out>>
    where
        T: 'a,
        ThrottledFn: FnOnce(T) -> BoxFuture<'a, Result<Out>> + Send + 'a,
    {
        let access = limiter.access();
        // NOTE: Make a clone of the Blobstore first then dispatch after the
        // limiter has allowed access, which ensures even eager work is delayed.
        let blobstore = self.blobstore.clone();
        async move {
            access.await?;
            throttled_fn(blobstore).await
        }
        .boxed()
    }
}

// All delegate to throttled_access, which ensures even eager methods are throttled
#[async_trait]
impl<T: Blobstore + Clone> Blobstore for ThrottledBlob<T> {
    async fn get(&self, ctx: CoreContext, key: String) -> Result<Option<BlobstoreGetData>> {
        self.throttled_access(&self.read_limiter, move |blobstore| {
            async move { blobstore.get(ctx, key).await }.boxed()
        })
        .await
    }

    async fn put(&self, ctx: CoreContext, key: String, value: BlobstoreBytes) -> Result<()> {
        self.throttled_access(&self.write_limiter, move |blobstore| {
            async move { blobstore.put(ctx, key, value).await }.boxed()
        })
        .await
    }

    async fn is_present(&self, ctx: CoreContext, key: String) -> Result<bool> {
        self.throttled_access(&self.read_limiter, move |blobstore| {
            async move { blobstore.is_present(ctx, key).await }.boxed()
        })
        .await
    }
}

// All delegate to throttled_access, which ensures even eager methods are throttled
#[async_trait]
impl<T: BlobstorePutOps + Clone> BlobstorePutOps for ThrottledBlob<T> {
    async fn put_explicit(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
        put_behaviour: PutBehaviour,
    ) -> Result<OverwriteStatus> {
        self.throttled_access(&self.write_limiter, move |blobstore| {
            async move { blobstore.put_explicit(ctx, key, value, put_behaviour).await }.boxed()
        })
        .await
    }

    async fn put_with_status(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<OverwriteStatus> {
        self.throttled_access(&self.write_limiter, move |blobstore| {
            async move { blobstore.put_with_status(ctx, key, value).await }.boxed()
        })
        .await
    }
}

impl<T: Clone + fmt::Debug> fmt::Debug for ThrottledBlob<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThrottledBlob")
            .field("blobstore", &self.blobstore)
            .field("options", &self.options)
            .finish()
    }
}
