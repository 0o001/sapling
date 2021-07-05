/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use async_trait::async_trait;
use blobstore::{Blobstore, BlobstoreGetData, BlobstoreIsPresent};
use context::CoreContext;
use mononoke_types::{BlobstoreBytes, RepositoryId};
use prefixblob::PrefixBlobstore;
use redactedblobstore::{RedactedBlobs, RedactedBlobstore, RedactedBlobstoreConfig};
use scuba_ext::MononokeScubaSampleBuilder;
use std::sync::Arc;

/// RedactedBlobstore should be part of every blobstore since it is a layer
/// which adds security by preventing users to access sensitive content.

/// Making PrefixBlobstore part of every blobstore does two things:
/// 1. It ensures that the prefix applies first, which is important for shared caches like
///    memcache.
/// 2. It ensures that all possible blobrepos use a prefix.
type RepoBlobstoreStack<T> = RedactedBlobstore<PrefixBlobstore<T>>;

// NOTE: We parametize AbstractRepoBlobstore over T instead of explicitly using Arc<dyn Blobstore>
// so that even if we were to add a blobstore to the RepoBlobstoreStack that actually is a Arc<dyn
// Blobstore>, then we cannot accidentally forget to unwrap it below (since we wouldn't get a T
// back).
#[derive(Clone, Debug)]
pub struct AbstractRepoBlobstore<T>(RepoBlobstoreStack<T>);

impl<T: Blobstore + Clone> AbstractRepoBlobstore<T> {
    pub fn as_parts(&self) -> (T, RedactedBlobstoreConfig) {
        let (blobstore, redacted_blobstore_config) = self.0.as_parts();

        (blobstore.into_inner(), redacted_blobstore_config)
    }
}

#[facet::facet]
#[derive(Clone, Debug)]
pub struct RepoBlobstore(AbstractRepoBlobstore<Arc<dyn Blobstore>>);

impl RepoBlobstore {
    pub fn as_parts(&self) -> (Arc<dyn Blobstore>, RedactedBlobstoreConfig) {
        self.0.as_parts()
    }

    pub fn boxed(&self) -> Arc<dyn Blobstore> {
        self.0.0.boxed()
    }
}

impl std::fmt::Display for RepoBlobstore {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "RepoBlobstore<{}>", self.0.0)
    }
}

#[async_trait]
impl Blobstore for RepoBlobstore {
    async fn get<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<Option<BlobstoreGetData>> {
        self.0.0.get(ctx, key).await
    }
    async fn put<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<()> {
        self.0.0.put(ctx, key, value).await
    }
    async fn is_present<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<BlobstoreIsPresent> {
        self.0.0.is_present(ctx, key).await
    }
}

pub struct RepoBlobstoreArgs {
    blobstore: RepoBlobstore,
    repoid: RepositoryId,
}

impl RepoBlobstoreArgs {
    pub fn new<T: Blobstore + 'static>(
        blobstore: T,
        redacted_blobs: Option<Arc<RedactedBlobs>>,
        repoid: RepositoryId,
        scuba_builder: MononokeScubaSampleBuilder,
    ) -> Self {
        let redacted_blobstore_config = RedactedBlobstoreConfig::new(redacted_blobs, scuba_builder);
        Self::build(blobstore, repoid, redacted_blobstore_config)
    }

    pub fn new_with_wrapped_inner_blobstore<T, F>(
        blobstore: RepoBlobstore,
        repoid: RepositoryId,
        wrapper: F,
    ) -> Self
    where
        T: Blobstore + 'static,
        F: FnOnce(Arc<dyn Blobstore>) -> T,
    {
        let (blobstore, redacted_blobstore_config) = blobstore.0.as_parts();
        let new_inner_blobstore = wrapper(blobstore);
        Self::build(new_inner_blobstore, repoid, redacted_blobstore_config)
    }

    pub fn into_blobrepo_parts(self) -> (RepoBlobstore, RepositoryId) {
        let Self { blobstore, repoid } = self;
        (blobstore, repoid)
    }

    pub fn repo_blobstore_clone(&self) -> RepoBlobstore {
        self.blobstore.clone()
    }

    fn build<T: Blobstore + 'static>(
        blobstore: T,
        repoid: RepositoryId,
        redacted_blobstore_config: RedactedBlobstoreConfig,
    ) -> Self {
        let blobstore: Arc<dyn Blobstore> = Arc::new(blobstore);
        let blobstore = PrefixBlobstore::new(blobstore, repoid.prefix());
        let blobstore = RedactedBlobstore::new(blobstore, redacted_blobstore_config);
        let blobstore = RepoBlobstore(AbstractRepoBlobstore(blobstore));

        Self { blobstore, repoid }
    }
}
