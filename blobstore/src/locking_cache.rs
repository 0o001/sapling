// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::fmt;
use std::sync::Arc;

use failure::Error;
use futures::{future, Future, IntoFuture, future::Either};
use futures_ext::{BoxFuture, FutureExt};
use tokio;

use context::CoreContext;
use mononoke_types::BlobstoreBytes;

use Blobstore;

/// Extra operations that can be performed on a cache. Other wrappers can implement this trait for
/// e.g. all `WrapperBlobstore<CacheBlobstore<T>>`.
///
/// This is primarily used by the admin command to manually check memcache.
pub trait CacheBlobstoreExt: Blobstore {
    fn get_no_cache_fill(
        &self,
        ctx: CoreContext,
        key: String,
    ) -> BoxFuture<Option<BlobstoreBytes>, Error>;
    fn get_cache_only(&self, key: String) -> BoxFuture<Option<BlobstoreBytes>, Error>;
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
    /// Fetch the blob from the cache, if possible. Return `None` if the cache does not have a
    /// copy of the blob (i.e. the cache entry is not in Known state).
    fn get(&self, key: &str) -> BoxFuture<Option<BlobstoreBytes>, ()>;

    /// Tell the cache that the backing store value for this `key` is `value`. This should put the
    /// cache entry for this `key` into Known state or a demotion of Known state (Present, Empty).
    fn put(&self, key: &str, value: BlobstoreBytes) -> BoxFuture<(), ()>;

    /// Ask the cache if it knows whether the backing store has a value for this key. Returns
    /// `true` if there is definitely a value (i.e. cache entry in Present or Known state), `false`
    /// otherwise (Empty or Leased states).
    fn check_present(&self, key: &str) -> BoxFuture<bool, ()>;
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
    fn try_add_put_lease(&self, key: &str) -> BoxFuture<bool, ()>;

    /// Wait for a suitable (cache-defined) period between `try_add_put_lease` attempts.
    /// For caches without a notification method, this should just be a suitable delay.
    /// For caches that can notify on key change, this should wait for that notification.
    /// It is acceptable to return from this future without checking the state of the cache entry.
    fn wait_for_other_leases(&self, key: &str) -> BoxFuture<(), ()>;

    /// Releases any leases held on `key`. `put_success` is a hint; if it is `true`, the entry
    /// can transition from Leased to either Present or Empty, while if it is `false`, the entry
    /// must transition from Leased to Empty.
    fn release_lease(&self, key: &str, put_success: bool) -> BoxFuture<(), ()>;
}

impl<C> CacheOps for Arc<C>
where
    C: CacheOps,
{
    fn get(&self, key: &str) -> BoxFuture<Option<BlobstoreBytes>, ()> {
        self.as_ref().get(key)
    }

    fn put(&self, key: &str, value: BlobstoreBytes) -> BoxFuture<(), ()> {
        self.as_ref().put(key, value)
    }

    fn check_present(&self, key: &str) -> BoxFuture<bool, ()> {
        self.as_ref().check_present(key)
    }
}

impl<L> LeaseOps for Arc<L>
where
    L: LeaseOps,
{
    fn try_add_put_lease(&self, key: &str) -> BoxFuture<bool, ()> {
        self.as_ref().try_add_put_lease(key)
    }

    fn wait_for_other_leases(&self, key: &str) -> BoxFuture<(), ()> {
        self.as_ref().wait_for_other_leases(key)
    }

    fn release_lease(&self, key: &str, put_success: bool) -> BoxFuture<(), ()> {
        self.as_ref().release_lease(key, put_success)
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
}

impl<C, L, T> CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    pub fn new(cache: C, lease: L, blobstore: T) -> Self {
        Self {
            blobstore,
            cache,
            lease,
        }
    }

    fn cache_get(
        &self,
        key: &str,
    ) -> impl Future<Item = Option<BlobstoreBytes>, Error = Error> + Send {
        self.cache.get(key).or_else(|_| Ok(None))
    }

    fn cache_put_closure(
        &self,
        key: &str,
    ) -> impl FnOnce(Option<BlobstoreBytes>) -> Option<BlobstoreBytes> {
        let key = key.to_string();
        let cache = self.cache.clone();

        move |value| {
            if let Some(ref value) = value {
                tokio::spawn(cache.put(&key, value.clone()));
            }
            value
        }
    }

    fn cache_put(
        &self,
        key: &str,
        value: BlobstoreBytes,
    ) -> impl Future<Item = (), Error = Error> + Send {
        let key = key.to_string();
        let cache = self.cache.clone();

        future::lazy(move || cache.put(&key, value).or_else(|_| Ok(()).into_future()))
    }

    fn cache_is_present(&self, key: &str) -> impl Future<Item = bool, Error = Error> + Send {
        self.cache.check_present(key).or_else(|_| Ok(false))
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
    fn get(&self, ctx: CoreContext, key: String) -> BoxFuture<Option<BlobstoreBytes>, Error> {
        let cache_get = self.cache_get(&key);
        let cache_put = self.cache_put_closure(&key);
        let blobstore_get = future::lazy({
            let blobstore = self.blobstore.clone();
            move || blobstore.get(ctx, key)
        });

        cache_get
            .and_then({
                move |blob| {
                    if blob.is_some() {
                        future::Either::A(Ok(blob).into_future())
                    } else {
                        future::Either::B(blobstore_get.map(cache_put))
                    }
                }
            })
            .boxify()
    }

    fn put(&self, ctx: CoreContext, key: String, value: BlobstoreBytes) -> BoxFuture<(), Error> {
        let can_put = self.take_put_lease(&key);
        let cache_put = self.cache_put(&key, value.clone())
            .join(future::lazy({
                let lease = self.lease.clone();
                let key = key.clone();
                move || lease.release_lease(&key, true).or_else(|_| Ok(()))
            }))
            .then(|_| Ok(()));

        let blobstore_put = future::lazy({
            let blobstore = self.blobstore.clone();
            let lease = self.lease.clone();
            let key = key.clone();
            move || {
                blobstore
                    .put(ctx, key.clone(), value)
                    .or_else(move |r| lease.release_lease(&key, false).then(|_| Err(r)))
            }
        });

        can_put
            .and_then(move |can_put| {
                if can_put {
                    Either::A(blobstore_put.and_then(move |_| cache_put))
                } else {
                    Either::B(Ok(()).into_future())
                }
            })
            .boxify()
    }

    fn is_present(&self, ctx: CoreContext, key: String) -> BoxFuture<bool, Error> {
        let cache_check = self.cache_is_present(&key);
        let blobstore_check = future::lazy({
            let blobstore = self.blobstore.clone();
            move || blobstore.is_present(ctx, key)
        });

        cache_check
            .and_then(|present| {
                if present {
                    Either::A(Ok(true).into_future())
                } else {
                    Either::B(blobstore_check)
                }
            })
            .boxify()
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
    ) -> BoxFuture<Option<BlobstoreBytes>, Error> {
        let cache_get = self.cache_get(&key);
        let blobstore_get = self.blobstore.get(ctx, key);

        cache_get
            .and_then(move |blob| {
                if blob.is_some() {
                    Ok(blob).into_future().boxify()
                } else {
                    blobstore_get.boxify()
                }
            })
            .boxify()
    }

    fn get_cache_only(&self, key: String) -> BoxFuture<Option<BlobstoreBytes>, Error> {
        self.cache_get(&key).boxify()
    }
}

impl<C, L, T> fmt::Debug for CacheBlobstore<C, L, T>
where
    C: CacheOps + Clone,
    L: LeaseOps + Clone,
    T: Blobstore + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("CacheBlobstore")
            .field("blobstore", &self.blobstore)
            .field("cache", &self.cache)
            .field("lease", &self.lease)
            .finish()
    }
}
