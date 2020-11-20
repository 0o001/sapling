/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use blobstore::{Blobstore, BlobstoreGetData, BlobstorePutOps, OverwriteStatus, PutBehaviour};
use context::CoreContext;
use futures::future::{BoxFuture, FutureExt, TryFutureExt};
use mononoke_types::BlobstoreBytes;
use rand::{thread_rng, Rng};
use std::num::NonZeroU32;

mod errors;
pub use crate::errors::ErrorKind;

const NEVER_CHAOS_THRESHOLD: f32 = 1.0;
const ALWAYS_CHAOS_THRESHOLD: f32 = -1.0;

#[derive(Clone, Copy, Debug)]
pub struct ChaosOptions {
    error_sample_read: Option<NonZeroU32>,
    error_sample_write: Option<NonZeroU32>,
}

impl ChaosOptions {
    /// Pass `error_sample_read` or `error_sample_write` value
    /// from Some(1) for always chaos to Some(N) to get 1/N chance of failure.
    pub fn new(
        error_sample_read: Option<NonZeroU32>,
        error_sample_write: Option<NonZeroU32>,
    ) -> Self {
        Self {
            error_sample_read,
            error_sample_write,
        }
    }

    pub fn has_chaos(&self) -> bool {
        self.error_sample_read.is_some() || self.error_sample_write.is_some()
    }
}

/// A layer over an existing blobstore that errors randomly
#[derive(Clone, Debug)]
pub struct ChaosBlobstore<T> {
    blobstore: T,
    sample_threshold_read: f32,
    sample_threshold_write: f32,
    options: ChaosOptions,
}

fn derive_threshold(sample_rate: Option<NonZeroU32>) -> f32 {
    sample_rate
        .map(|rate| {
            match rate.get() {
                // Avoid chance of rng returning 0.0 and threshold being 0.0
                1 => ALWAYS_CHAOS_THRESHOLD,
                // If rate 100, then rng must generate over 0.99 to trigger error
                n => 1.0 - (1.0 / (n as f32)),
            }
        })
        .unwrap_or(NEVER_CHAOS_THRESHOLD)
}

impl<T> ChaosBlobstore<T> {
    pub fn new(blobstore: T, options: ChaosOptions) -> Self {
        let sample_threshold_read = derive_threshold(options.error_sample_read);
        let sample_threshold_write = derive_threshold(options.error_sample_write);
        Self {
            blobstore,
            sample_threshold_read,
            sample_threshold_write,
            options,
        }
    }
}

impl<T: Blobstore + BlobstorePutOps> Blobstore for ChaosBlobstore<T> {
    #[inline]
    fn get(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture<'_, Result<Option<BlobstoreGetData>>> {
        let should_error = thread_rng().gen::<f32>() > self.sample_threshold_read;
        let get = self.blobstore.get(ctx, key.clone());
        async move {
            if should_error {
                Err(ErrorKind::InjectedChaosGet(key).into())
            } else {
                get.await
            }
        }
        .boxed()
    }

    #[inline]
    fn put(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> BoxFuture<'_, Result<()>> {
        self.put_impl(ctx, key, value, None).map_ok(|_| ()).boxed()
    }

    #[inline]
    fn is_present(&self, ctx: CoreContext, key: String) -> BoxFuture<'_, Result<bool>> {
        let should_error = thread_rng().gen::<f32>() > self.sample_threshold_read;
        let is_present = self.blobstore.is_present(ctx, key.clone());
        async move {
            if should_error {
                Err(ErrorKind::InjectedChaosIsPresent(key).into())
            } else {
                is_present.await
            }
        }
        .boxed()
    }
}

impl<T: BlobstorePutOps> ChaosBlobstore<T> {
    fn put_impl(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
        put_behaviour: Option<PutBehaviour>,
    ) -> BoxFuture<'_, Result<OverwriteStatus>> {
        let should_error = thread_rng().gen::<f32>() > self.sample_threshold_write;
        let put = if should_error {
            None
        } else {
            let put = if let Some(put_behaviour) = put_behaviour {
                self.blobstore
                    .put_explicit(ctx, key.clone(), value, put_behaviour)
            } else {
                self.blobstore.put_with_status(ctx, key.clone(), value)
            };
            Some(put)
        };
        async move {
            match put {
                None => Err(ErrorKind::InjectedChaosPut(key).into()),
                Some(put) => put.await,
            }
        }
        .boxed()
    }
}

impl<T: BlobstorePutOps> BlobstorePutOps for ChaosBlobstore<T> {
    fn put_explicit(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
        put_behaviour: PutBehaviour,
    ) -> BoxFuture<'_, Result<OverwriteStatus>> {
        self.put_impl(ctx, key, value, Some(put_behaviour))
    }

    fn put_with_status(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> BoxFuture<'_, Result<OverwriteStatus>> {
        self.put_impl(ctx, key, value, None)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fbinit::FacebookInit;

    use memblob::EagerMemblob;

    #[fbinit::compat_test]
    async fn test_error_on_write(fb: FacebookInit) {
        let ctx = CoreContext::test_mock(fb);
        let base = EagerMemblob::default();
        let wrapper =
            ChaosBlobstore::new(base.clone(), ChaosOptions::new(None, NonZeroU32::new(1)));
        let key = "foobar".to_string();

        let r = wrapper
            .put(
                ctx.clone(),
                key.clone(),
                BlobstoreBytes::from_bytes("test foobar"),
            )
            .await;
        assert!(!r.is_ok());
        let base_present = base.is_present(ctx, key.clone()).await.unwrap();
        assert!(!base_present);
    }

    #[fbinit::compat_test]
    async fn test_error_on_write_with_status(fb: FacebookInit) {
        let ctx = CoreContext::test_mock(fb);
        let base = EagerMemblob::default();
        let wrapper =
            ChaosBlobstore::new(base.clone(), ChaosOptions::new(None, NonZeroU32::new(1)));
        let key = "foobar".to_string();

        let r = wrapper
            .put_with_status(
                ctx.clone(),
                key.clone(),
                BlobstoreBytes::from_bytes("test foobar"),
            )
            .await;
        assert!(!r.is_ok());
        let base_present = base.is_present(ctx, key.clone()).await.unwrap();
        assert!(!base_present);
    }

    #[fbinit::compat_test]
    async fn test_error_on_read(fb: FacebookInit) {
        let ctx = CoreContext::test_mock(fb);
        let base = EagerMemblob::default();
        let wrapper =
            ChaosBlobstore::new(base.clone(), ChaosOptions::new(NonZeroU32::new(1), None));
        let key = "foobar".to_string();

        let r = wrapper
            .put(
                ctx.clone(),
                key.clone(),
                BlobstoreBytes::from_bytes("test foobar"),
            )
            .await;
        assert!(r.is_ok());
        let base_present = base.is_present(ctx.clone(), key.clone()).await.unwrap();
        assert!(base_present);
        let r = wrapper.get(ctx.clone(), key.clone()).await;
        assert!(!r.is_ok());
    }
}
