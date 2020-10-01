/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use blobstore::{Blobstore, BlobstoreGetData};
use context::CoreContext;
use futures::future::BoxFuture;
use mononoke_types::BlobstoreBytes;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct TracingBlobstore<T> {
    inner: T,
    gets: Arc<Mutex<Vec<String>>>,
}

impl<T> TracingBlobstore<T> {
    pub fn new(inner: T) -> Self {
        let gets = Arc::new(Mutex::new(vec![]));
        Self { inner, gets }
    }
}

impl<T> TracingBlobstore<T> {
    pub fn tracing_gets(&self) -> Vec<String> {
        let mut gets = self.gets.lock().expect("poisoned lock");
        std::mem::replace(&mut *gets, vec![])
    }
}

impl<T> Blobstore for TracingBlobstore<T>
where
    T: Blobstore,
{
    fn get(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture<'static, Result<Option<BlobstoreGetData>, Error>> {
        let mut gets = self.gets.lock().expect("poisoned lock");
        gets.push(key.clone());

        self.inner.get(ctx, key)
    }

    fn put(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.inner.put(ctx, key, value)
    }

    fn is_present(&self, ctx: CoreContext, key: String) -> BoxFuture<'static, Result<bool, Error>> {
        self.inner.is_present(ctx, key)
    }
}
