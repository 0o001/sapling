/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use blobstore::{Blobstore, BlobstoreGetData, CountedBlobstore};
use cloned::cloned;
use context::{CoreContext, PerfCounterType};
use futures::{
    compat::Future01CompatExt,
    future::{BoxFuture, FutureExt, TryFutureExt},
};
use futures_ext::{BoxFuture as BoxFuture01, FutureExt as OldFutureExt};
use futures_old::{future, future::Either, Future, IntoFuture};
use mononoke_types::BlobstoreBytes;
use prefixblob::PrefixBlobstore;
use redactedblobstore::{config::GET_OPERATION, RedactedBlobstore};
use stats::prelude::*;
use std::fmt;
use std::sync::Arc;

define_stats! {
    prefix = "mononoke.blobstore.cacheblob";
    get_miss: dynamic_timeseries("{}.get_miss", (cache_name: &'static str); Rate, Sum),
    get_hit: dynamic_timeseries("{}.get_hit", (cache_name: &'static str); Rate, Sum),
    presence_hit: dynamic_timeseries("{}.presence_hit", (cache_name: &'static str); Rate, Sum),
    presence_miss: dynamic_timeseries("{}.presence_miss", (cache_name: &'static str); Rate, Sum),
}

/// Extra operations that can be performed on a cache. Other wrappers can implement this trait for
/// e.g. all `WrapperBlobstore<CacheBlobstore<T>>`.
///
/// This is primarily used by the admin command to manually check memcache.
pub trait CacheBlobstoreExt: Blobstore {
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error>;
    fn get_cache_only(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error>;
}

/// The operations a cache must provide in order to be usable as the caching layer for a
/// caching blobstore that caches blob contents and blob presence.
/// For caches that do no I/O (e.g. in-memory caches), use Result::into_future() to create the
/// return types - it is up to CacheBlobstore to use future::lazy where this would be unacceptable
/// Errors returned by the cache are always ignored.
///
/// The cache is expected to act as-if each entry is in one of four states:
/// 1. Empty, implying that the cache has no knowledge of the backing store state for this key.
/// 2. Leased, implying that the cache is aware that there is an attempt being made to update the
///    backing store for this key.
/// 3. Present, implying that the cache is aware that a backing store entry exists for this key
///    but does not have a copy of the blob.
/// 4. Known, implying that the cache has a copy of the blob for this key.
///
/// When the cache engages in eviction, it demotes entries according to the following plan:
/// Present and Leased can only demote to Empty.
/// Known can demote to Present or Empty.
/// No state is permitted to demote to Leased.
/// Caches that do not support LeaseOps do not have the Leased state.
pub trait CacheOps: fmt::Debug + Send + Sync + 'static {
    const HIT_COUNTER: Option<PerfCounterType> = None;
    const MISS_COUNTER: Option<PerfCounterType> = None;
    const CACHE_NAME: &'static str = "unknown";

    /// Fetch the blob from the cache, if possible. Return `None` if the cache does not have a
    /// copy of the blob (i.e. the cache entry is not in Known state).
    fn get(&self, key: &str) -> BoxFuture01<Option<BlobstoreGetData>, ()>;

    /// Tell the cache that the backing store value for this `key` is `value`. This should put the
    /// cache entry for this `key` into Known state or a demotion of Known state (Present, Empty).
    fn put(&self, key: &str, value: BlobstoreGetData) -> BoxFuture01<(), ()>;

    /// Ask the cache if it knows whether the backing store has a value for this key. Returns
    /// `true` if there is definitely a value (i.e. cache entry in Present or Known state), `false`
    /// otherwise (Empty or Leased states).
    fn check_present(&self, key: &str) -> BoxFuture01<bool, ()>;
}

/// The operations a cache must provide to take part in the update lease protocol. This reduces the
/// thundering herd on writes by using the Leased state to ensure that only one user of this cache
/// can write to the backing store at any time. Note that this is not a guarantee that there will
/// be only one writer to the backing store for any given key - notably, the cache can demote
/// Leased to Empty, thus letting another writer that shares the same cache through to the backing
/// store.
pub trait LeaseOps: fmt::Debug + Send + Sync + 'static {
    /// Ask the cache to attempt to lock out other users of this cache for a particular key.
    /// This is an atomic test-and-set of the cache entry; it tests that the entry is Empty, and if
    /// the entry is Empty, it changes it to the Leased state.
    /// The result is `true` if the test-and-set changed the entry to Leased state, `false`
    /// otherwise
    fn try_add_put_lease(&self, key: &str) -> BoxFuture01<bool, ()>;

    /// Will keep the lease alive until `done` future resolves.
    /// Note that it should only be called after successful try_add_put_lease()
    fn renew_lease_until(&self, ctx: CoreContext, key: &str, done: BoxFuture01<(), ()>);

    /// Wait for a suitable (cache-defined) period between `try_add_put_lease` attempts.
    /// For caches without a notification method, this should just be a suitable delay.
    /// For caches that can notify on key change, this should wait for that notification.
    /// It is acceptable to return from this future without checking the state of the cache entry.
    fn wait_for_other_leases(&self, key: &str) -> BoxFuture01<(), ()>;

    /// Releases any leases held on `key`. The entry must transition from Leased to Empty.
    fn release_lease(&self, key: &str) -> BoxFuture01<(), ()>;
}

impl<C> CacheOps for Arc<C>
where
    C: ?Sized + CacheOps,
{
    fn get(&self, key: &str) -> BoxFuture01<Option<BlobstoreGetData>, ()> {
        self.as_ref().get(key)
    }

    fn put(&self, key: &str, value: BlobstoreGetData) -> BoxFuture01<(), ()> {
        self.as_ref().put(key, value)
    }

    fn check_present(&self, key: &str) -> BoxFuture01<bool, ()> {
        self.as_ref().check_present(key)
    }
}

impl<L> LeaseOps for Arc<L>
where
    L: LeaseOps,
{
    fn try_add_put_lease(&self, key: &str) -> BoxFuture01<bool, ()> {
        self.as_ref().try_add_put_lease(key)
    }

    fn renew_lease_until(&self, ctx: CoreContext, key: &str, done: BoxFuture01<(), ()>) {
        self.as_ref().renew_lease_until(ctx, key, done)
    }

    fn wait_for_other_leases(&self, key: &str) -> BoxFuture01<(), ()> {
        self.as_ref().wait_for_other_leases(key)
    }

    fn release_lease(&self, key: &str) -> BoxFuture01<(), ()> {
        self.as_ref().release_lease(key)
    }
}

pub struct CacheOpsUtil {}

impl CacheOpsUtil {
    pub fn get<C: CacheOps>(
        cache: &C,
        key: &str,
    ) -> impl Future<Item = Option<BlobstoreGetData>, Error = Error> + Send {
        cache.get(key).or_else(|_| Ok(None))
    }

    pub fn put_closure<C: CacheOps + Clone>(
        cache: &C,
        key: &str,
    ) -> impl Fn(Option<BlobstoreGetData>) -> Option<BlobstoreGetData> {
        let key = key.to_string();
        let cache = cache.clone();

        move |value| {
            if let Some(ref value) = value {
                tokio_old::spawn(cache.put(&key, value.clone()));
            }
            value
        }
    }

    pub fn put<C: CacheOps + Clone>(
        cache: &C,
        key: &str,
        value: BlobstoreGetData,
    ) -> impl Future<Item = (), Error = ()> + Send {
        let key = key.to_string();
        let cache = cache.clone();

        future::lazy(move || cache.put(&key, value).or_else(|_| Ok(()).into_future()))
    }

    pub fn is_present<C: CacheOps>(
        cache: &C,
        key: &str,
    ) -> impl Future<Item = bool, Error = Error> + Send {
        cache.check_present(key).or_else(|_| Ok(false))
    }
}

/// A caching layer over a blobstore, using a cache defined by its CacheOps. The idea is that
/// generic code that any caching layer needs is defined here, while code that's cache-specific
/// goes into CacheOps
#[derive(Clone)]
pub struct CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    blobstore: T,
    cache: C,
    lease: L,
    lazy_cache_put: bool,
}

impl<C, L, T> CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    pub fn new(cache: C, lease: L, blobstore: T, lazy_cache_put: bool) -> Self {
        Self {
            blobstore,
            cache,
            lease,
            lazy_cache_put,
        }
    }

    fn take_put_lease(&self, key: &str) -> impl Future<Item = bool, Error = Error> + Send {
        self.lease
            .try_add_put_lease(key)
            .or_else(|_| Ok(false))
            .and_then({
                let cache = self.cache.clone();
                let lease = self.lease.clone();
                let this = self.clone();
                let key = key.to_string();

                move |leased| {
                    if leased {
                        Either::A(Ok(true).into_future())
                    } else {
                        Either::B(cache.check_present(&key).or_else(|_| Ok(false)).and_then(
                            move |present| {
                                if present {
                                    Either::A(Ok(false).into_future())
                                } else {
                                    Either::B(
                                        lease
                                            .wait_for_other_leases(&key)
                                            .then(move |_| this.take_put_lease(&key).boxify()),
                                    )
                                }
                            },
                        ))
                    }
                }
            })
    }
}

impl<C, L, T> Blobstore for CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    fn get(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture<'static, Result<Option<BlobstoreGetData>, Error>> {
        let cache_get = CacheOpsUtil::get(&self.cache, &key);
        let cache_put = CacheOpsUtil::put_closure(&self.cache, &key);

        cache_get
            .and_then({
                cloned!(self.blobstore);
                move |blob| {
                    if blob.is_some() {
                        if let Some(counter) = C::HIT_COUNTER {
                            ctx.perf_counters().increment_counter(counter);
                        }
                        STATS::get_hit.add_value(1, (C::CACHE_NAME,));
                        future::Either::A(Ok(blob).into_future())
                    } else {
                        if let Some(counter) = C::MISS_COUNTER {
                            ctx.perf_counters().increment_counter(counter);
                        }
                        STATS::get_miss.add_value(1, (C::CACHE_NAME,));
                        future::Either::B(blobstore.get(ctx, key).compat().map(cache_put))
                    }
                }
            })
            .compat()
            .boxed()
    }

    fn put(
        &self,
        ctx: CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> BoxFuture<'static, Result<(), Error>> {
        let can_put = self.take_put_lease(&key).compat();
        let cache_put = CacheOpsUtil::put(&self.cache, &key, value.clone().into());

        cloned!(self.blobstore, self.lease, self.lazy_cache_put);
        async move {
            if can_put.await? {
                let () = blobstore.put(ctx, key.clone(), value).await?;

                let cache_put = cache_put
                    .then(move |_: Result<(), ()>| lease.release_lease(&key))
                    .compat();
                if lazy_cache_put {
                    tokio::spawn(cache_put);
                } else {
                    let _ = cache_put.await;
                }
            }
            Ok(())
        }
        .boxed()
    }

    fn is_present(&self, ctx: CoreContext, key: String) -> BoxFuture<'static, Result<bool, Error>> {
        let cache_check = CacheOpsUtil::is_present(&self.cache, &key);
        let blobstore_check = future::lazy({
            let blobstore = self.blobstore.clone();
            move || blobstore.is_present(ctx, key).compat()
        });

        cache_check
            .and_then(|present| {
                if present {
                    STATS::presence_hit.add_value(1, (C::CACHE_NAME,));
                    Either::A(Ok(true).into_future())
                } else {
                    STATS::presence_miss.add_value(1, (C::CACHE_NAME,));
                    Either::B(blobstore_check)
                }
            })
            .compat()
            .boxed()
    }
}

impl<C, L, T> CacheBlobstoreExt for CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        let cache_get = CacheOpsUtil::get(&self.cache, &key);
        let blobstore_get = self.blobstore.get(ctx, key);

        cache_get
            .and_then(move |blob| {
                if blob.is_some() {
                    Ok(blob).into_future().boxify()
                } else {
                    blobstore_get.compat().boxify()
                }
            })
            .boxify()
    }

    fn get_cache_only(
        &self,
        _ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        CacheOpsUtil::get(&self.cache, &key).boxify()
    }
}

impl<C, L, T> fmt::Debug for CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheBlobstore")
            .field("blobstore", &self.blobstore)
            .field("cache", &self.cache)
            .field("lease", &self.lease)
            .finish()
    }
}

impl<T: CacheBlobstoreExt> CacheBlobstoreExt for CountedBlobstore<T> {
    #[inline]
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.as_inner().get_no_cache_fill(ctx, key)
    }

    #[inline]
    fn get_cache_only(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.as_inner().get_cache_only(ctx, key)
    }
}

impl<T: CacheBlobstoreExt + Clone> CacheBlobstoreExt for PrefixBlobstore<T> {
    #[inline]
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.as_inner().get_no_cache_fill(ctx, self.prepend(key))
    }

    #[inline]
    fn get_cache_only(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.as_inner().get_cache_only(ctx, self.prepend(key))
    }
}

impl<T: CacheBlobstoreExt + Clone> CacheBlobstoreExt for RedactedBlobstore<T> {
    #[inline]
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.access_blobstore(&ctx, &key, GET_OPERATION)
            .map(move |blobstore| blobstore.get_no_cache_fill(ctx, key))
            .into_future()
            .flatten()
            .boxify()
    }

    #[inline]
    fn get_cache_only(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture01<Option<BlobstoreGetData>, Error> {
        self.access_blobstore(&ctx, &key, GET_OPERATION)
            .map(move |blobstore| blobstore.get_cache_only(ctx, key))
            .into_future()
            .flatten()
            .boxify()
    }
}
