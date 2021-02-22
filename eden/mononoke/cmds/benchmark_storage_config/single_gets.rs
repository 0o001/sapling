/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput};
use rand::{thread_rng, Rng, RngCore};
use tokio::runtime::Runtime;

use blobstore::{Blobstore, BlobstoreBytes};
use context::CoreContext;

use crate::{KB, MB};

pub fn benchmark(
    c: &mut Criterion,
    ctx: CoreContext,
    blobstore: Arc<dyn Blobstore>,
    runtime: &mut Runtime,
) {
    let mut group = c.benchmark_group("single_gets");

    for size in [128, 16 * KB, 512 * KB, 8 * MB].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), size, |b, &size| {
            let mut block = vec![0; size];
            thread_rng().fill(&mut block as &mut [u8]);

            let block = BlobstoreBytes::from_bytes(block);
            let key = format!("benchmark.{:x}", thread_rng().next_u64());
            runtime.block_on(async {
                blobstore
                    .put(&ctx, key.clone(), block)
                    .await
                    .expect("Put failed")
            });
            let test = |ctx, blobstore: Arc<dyn Blobstore>| {
                let key = key.clone();
                async move {
                    blobstore.get(&ctx, &key).await.expect("Get failed");
                }
            };
            b.iter(|| runtime.block_on(async { test(ctx.clone(), Arc::clone(&blobstore)).await }));
        });
    }
    group.finish();
}
