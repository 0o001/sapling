/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Error, Result};
use blobstore::Blobstore;
use bytes::Bytes;
use context::CoreContext;
use fbinit::FacebookInit;
use futures::{
    future::{self, TryFutureExt},
    stream,
};
use quickcheck::{Arbitrary, StdGen};
use std::collections::HashSet;
use std::sync::Arc;

use crate as filestore;
use crate::incremental_hash::{
    hash_bytes, ContentIdIncrementalHasher, GitSha1IncrementalHasher, Sha1IncrementalHasher,
    Sha256IncrementalHasher,
};
use crate::{Alias, FetchKey, FilestoreConfig};

use super::failing_blobstore::FailingBlobstore;
use super::request;

/// Fetching through any alias should return the same outcome.
async fn check_consistency<B: Blobstore>(
    blobstore: &B,
    ctx: CoreContext,
    bytes: &Bytes,
) -> Result<bool, Error> {
    let content_id = hash_bytes(ContentIdIncrementalHasher::new(), &bytes);
    let sha1 = hash_bytes(Sha1IncrementalHasher::new(), &bytes);
    let sha256 = hash_bytes(Sha256IncrementalHasher::new(), &bytes);
    let git_sha1 = hash_bytes(GitSha1IncrementalHasher::new(*&bytes), &bytes);

    let content_id = FetchKey::Canonical(content_id);
    let sha1 = FetchKey::Aliased(Alias::Sha1(sha1));
    let sha256 = FetchKey::Aliased(Alias::Sha256(sha256));
    let git_sha1 = FetchKey::Aliased(Alias::GitSha1(git_sha1.sha1()));

    let futs = vec![
        filestore::fetch(blobstore, ctx.clone(), &content_id),
        filestore::fetch(blobstore, ctx.clone(), &sha1),
        filestore::fetch(blobstore, ctx.clone(), &sha256),
        filestore::fetch(blobstore, ctx.clone(), &git_sha1),
    ];

    let futs: Vec<_> = futs
        .into_iter()
        .map(|f| f.map_ok(|r| r.is_some()))
        .collect();

    let outcomes = future::try_join_all(futs).await?;

    // Either all should exist, or none should exist.
    let h: HashSet<_> = outcomes.iter().collect();
    if h.len() == 1 {
        Ok(*h.into_iter().next().unwrap())
    } else {
        Err(format_err!("Inconsistent fetch results: {:?}", outcomes))
    }
}

async fn check_metadata<B: Blobstore>(
    blobstore: &B,
    ctx: CoreContext,
    bytes: &Bytes,
) -> Result<bool, Error> {
    let content_id = hash_bytes(ContentIdIncrementalHasher::new(), &bytes);

    filestore::get_metadata(blobstore, ctx.clone(), &FetchKey::Canonical(content_id))
        .await
        .map(|r| r.is_some())
}

#[fbinit::test]
fn test_invariants(fb: FacebookInit) -> Result<()> {
    // NOTE: We make calls to our Blobstore succeed with 75% probability below. This might seem
    // high, but this actually makes most store() calls fail, since there is a lot that needs to go
    // right for a store() call to succeed (all the chunks need to be saved, then we need to write
    // 3 aliases, and then the content).
    let mut rt = tokio_compat::runtime::Runtime::new()?;
    let mut gen = StdGen::new(rand::thread_rng(), 128);

    let memblob = Arc::new(memblob::LazyMemblob::default());
    let blob = FailingBlobstore::new(memblob.clone(), 0.75, 0.75);
    let config = FilestoreConfig {
        chunk_size: Some(16),
        concurrency: 5,
    };
    let ctx = CoreContext::test_mock(fb);

    for _ in 0..1000 {
        let bytes = Bytes::from(Vec::arbitrary(&mut gen));
        let req = request(&bytes);

        // Try to store with a broken blobstore. It doesn't matter if we succeed or not.
        let res = rt.block_on_std(filestore::store(
            &blob,
            config,
            ctx.clone(),
            &req,
            stream::once(future::ready(Ok(bytes.clone()))),
        ));
        println!("store: {:?}", res);

        // Try to read with a functional blobstore. All results should be consistent.
        let content_ok = rt.block_on_std(check_consistency(&memblob, ctx.clone(), &bytes))?;
        println!("content_ok: {:?}", content_ok);

        // If we can read the content metadata, then we should also be able to read a metadata.
        let metadata_ok = rt.block_on_std(check_metadata(&memblob, ctx.clone(), &bytes))?;
        println!("metadata_ok: {:?}", metadata_ok);
        assert_eq!(content_ok, metadata_ok)
    }

    Ok(())
}

#[fbinit::test]
fn test_store_bytes_consistency(fb: FacebookInit) -> Result<(), Error> {
    async_unit::tokio_unit_test(async move {
        let mut gen = StdGen::new(rand::thread_rng(), 128);

        let memblob = Arc::new(memblob::LazyMemblob::default());
        let ctx = CoreContext::test_mock(fb);

        for _ in 0..100usize {
            let bytes = Bytes::from(Vec::arbitrary(&mut gen));

            let no_chunking = FilestoreConfig {
                chunk_size: None,
                concurrency: 1,
            };

            let chunked = FilestoreConfig {
                chunk_size: Some(std::cmp::max(1, (bytes.len() as u64) / 2)),
                concurrency: 1,
            };

            let too_small_to_chunk = FilestoreConfig {
                chunk_size: Some(std::cmp::max(1, (bytes.len() as u64) * 2)),
                concurrency: 1,
            };

            let ((id1, len1), fut1) =
                filestore::store_bytes(&memblob, no_chunking, ctx.clone(), bytes.clone());
            fut1.await?;

            assert_eq!(
                bytes,
                filestore::fetch_concat(&memblob, ctx.clone(), id1).await?
            );

            let ((id2, len2), fut2) =
                filestore::store_bytes(&memblob, chunked, ctx.clone(), bytes.clone());
            fut2.await?;

            assert_eq!(
                bytes,
                filestore::fetch_concat(&memblob, ctx.clone(), id2).await?
            );

            let ((id3, len3), fut3) =
                filestore::store_bytes(&memblob, too_small_to_chunk, ctx.clone(), bytes.clone());
            fut3.await?;

            assert_eq!(
                bytes,
                filestore::fetch_concat(&memblob, ctx.clone(), id3).await?
            );

            let meta = filestore::store(
                &memblob,
                no_chunking,
                ctx.clone(),
                &request(&bytes),
                stream::once(future::ready(Ok(bytes.clone()))),
            )
            .await?;

            assert_eq!(meta.content_id, id1);
            assert_eq!(meta.content_id, id2);
            assert_eq!(meta.content_id, id3);

            assert_eq!(meta.total_size, len1);
            assert_eq!(meta.total_size, len2);
            assert_eq!(meta.total_size, len3);
        }

        Result::<_, Error>::Ok(())
    })
}
