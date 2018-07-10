// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::time::Duration;

use failure::{err_msg, Error};
use futures::{Future, IntoFuture, future::Either};
use futures_ext::{BoxFuture, FutureExt};
use memcache::{KeyGen, MemcacheClient};
use rust_thrift::compact_protocol;
use tokio_timer::Timer;

use fbwhoami::FbWhoAmI;
use mononoke_types::BlobstoreBytes;
use stats::Timeseries;

use Blobstore;
use CacheBlobstore;
use CacheOps;
use CountedBlobstore;
use LeaseOps;
use memcache_lock_thrift::LockState;

define_stats! {
    prefix = "mononoke.blobstore.memcache";
    blob_put: timeseries("blob_put"; RATE, SUM),
    blob_put_err: timeseries("blob_put_err"; RATE, SUM),
    presence_put: timeseries("presence_put"; RATE, SUM),
    presence_put_err: timeseries("presence_put_err"; RATE, SUM),
    lease_claim: timeseries("lease_claim"; RATE, SUM),
    lease_claim_err: timeseries("lease_claim_err"; RATE, SUM),
    lease_conflict: timeseries("lease_conflict"; RATE, SUM),
    lease_wait_ms: timeseries("lease_wait_ms"; RATE, SUM),
    lease_release: timeseries("lease_release"; RATE, SUM),
    lease_release_good: timeseries("lease_release_good"; RATE, SUM),
    lease_release_bad: timeseries("lease_release_bad"; RATE, SUM),
    blob_presence: timeseries("blob_presence"; RATE, SUM),
    blob_presence_hit: timeseries("blob_presence_hit"; RATE, SUM),
    blob_presence_miss: timeseries("blob_presence_miss"; RATE, SUM),
    blob_presence_err: timeseries("blob_presence_err"; RATE, SUM),
    presence_get: timeseries("presence_get"; RATE, SUM),
    presence_check_hit: timeseries("presence_check_hit"; RATE, SUM),
    presence_check_miss: timeseries("presence_check_miss"; RATE, SUM),
    // This can come from leases as well as presence checking.
    presence_err: timeseries("presence_err"; RATE, SUM),
}

/// A caching layer over an existing blobstore, backed by memcache
#[derive(Clone)]
pub struct MemcacheOps {
    memcache: MemcacheClient,
    timer: Timer,
    keygen: KeyGen,
    presence_keygen: KeyGen,
    hostname: String,
}

const MEMCACHE_MAX_SIZE: usize = 1024000;
const MC_CODEVER: u32 = 0;
const MC_SITEVER: u32 = 0;

fn mc_raw_put(
    memcache: MemcacheClient,
    orig_key: String,
    key: String,
    value: BlobstoreBytes,
    presence_key: String,
) -> impl Future<Item = (), Error = ()> {
    let uploaded = compact_protocol::serialize(&LockState::uploaded_key(orig_key));

    STATS::presence_put.add_value(1);
    memcache.set(presence_key, uploaded).then(move |res| {
        if let Err(_) = res {
            STATS::presence_put_err.add_value(1);
        }
        if value.len() < MEMCACHE_MAX_SIZE {
            STATS::blob_put.add_value(1);
            Either::A(memcache.set(key, value.into_bytes()).or_else(|_| {
                STATS::blob_put_err.add_value(1);
                Ok(()).into_future()
            }))
        } else {
            Either::B(Ok(()).into_future())
        }
    })
}

impl MemcacheOps {
    pub fn new<S>(backing_store_name: S, backing_store_params: S) -> Result<Self, Error>
    where
        S: AsRef<str>,
    {
        let hostname = FbWhoAmI::new()?
            .get_name()
            .ok_or(err_msg("No hostname in fbwhoami"))?
            .to_string();

        let backing_store_name = backing_store_name.as_ref();
        let blob_key = "scm.mononoke.blobstore.".to_string() + backing_store_name.as_ref() + "."
            + backing_store_params.as_ref();
        let presence_key = "scm.mononoke.blobstore.presence.".to_string()
            + backing_store_name.as_ref() + "."
            + backing_store_params.as_ref();

        Ok(Self {
            memcache: MemcacheClient::new(),
            timer: Timer::default(),
            keygen: KeyGen::new(blob_key, MC_CODEVER, MC_SITEVER),
            presence_keygen: KeyGen::new(presence_key, MC_CODEVER, MC_SITEVER),
            hostname,
        })
    }

    fn get_lock_state(
        &self,
        key: String,
    ) -> impl Future<Item = Option<LockState>, Error = ()> + Send {
        let mc_key = self.presence_keygen.key(key.clone());
        STATS::presence_get.add_value(1);
        self.memcache
            .get(mc_key.clone())
            .and_then({
                let mc = self.memcache.clone();
                move |opt_blob| {
                    let opt_res = opt_blob
                        .and_then(|blob| compact_protocol::deserialize(Vec::from(blob)).ok());

                    if let Some(LockState::uploaded_key(up_key)) = &opt_res {
                        if key != *up_key {
                            // The lock state is invalid - fix it up by dropping the lock
                            return Either::A(mc.del(mc_key).map(|_| None));
                        }
                    }

                    Either::B(Ok(opt_res).into_future())
                }
            })
            .or_else(move |_| {
                STATS::presence_err.add_value(1);
                Ok(None).into_future()
            })
    }
}

pub fn new_memcache_blobstore<T, S>(
    blobstore: T,
    backing_store_name: S,
    backing_store_params: S,
) -> Result<CountedBlobstore<CacheBlobstore<MemcacheOps, MemcacheOps, T>>, Error>
where
    T: Blobstore + Clone,
    S: AsRef<str>,
{
    let cache_ops = MemcacheOps::new(backing_store_name, backing_store_params)?;
    Ok(CountedBlobstore::new(
        "memcache",
        CacheBlobstore::new(cache_ops.clone(), cache_ops, blobstore),
    ))
}

impl CacheOps for MemcacheOps {
    // Turns errors to Ok(None)
    fn get(&self, key: &str) -> BoxFuture<Option<BlobstoreBytes>, ()> {
        let mc_key = self.keygen.key(key);
        self.memcache
            .get(mc_key)
            .map(|buf| buf.map(|buf| BlobstoreBytes::from_bytes(buf)))
            .boxify()
    }

    fn put(&self, key: &str, value: BlobstoreBytes) -> BoxFuture<(), ()> {
        let mc_key = self.keygen.key(key);
        let presence_key = self.presence_keygen.key(key);
        let orig_key = key.to_string();

        mc_raw_put(self.memcache.clone(), orig_key, mc_key, value, presence_key).boxify()
    }

    fn check_present(&self, key: &str) -> BoxFuture<bool, ()> {
        let lock_presence = self.get_lock_state(key.to_string()).map({
            move |lockstate| match lockstate {
                // get_lock_state will delete the lock and return None if there's a bad
                // uploaded_key
                Some(LockState::uploaded_key(_)) => {
                    STATS::presence_check_hit.add_value(1);
                    true
                }
                _ => {
                    STATS::presence_check_miss.add_value(1);
                    false
                }
            }
        });

        let mc_key = self.keygen.key(key);
        STATS::blob_presence.add_value(1);
        let blob_presence = self.memcache
            .get(mc_key)
            .map(|blob| blob.is_some())
            .then(move |res| {
                match res {
                    Ok(true) => STATS::blob_presence_hit.add_value(1),
                    Ok(false) => STATS::blob_presence_miss.add_value(1),
                    Err(_) => STATS::blob_presence_err.add_value(1),
                };
                res
            });

        lock_presence
            .and_then(move |present| {
                if present {
                    Either::A(Ok(true).into_future())
                } else {
                    Either::B(blob_presence)
                }
            })
            .boxify()
    }
}

impl LeaseOps for MemcacheOps {
    fn try_add_put_lease(&self, key: &str) -> BoxFuture<bool, ()> {
        let lockstate = compact_protocol::serialize(&LockState::locked_by(self.hostname.clone()));
        let lock_ttl = Duration::from_secs(10);
        let mc_key = self.presence_keygen.key(key);

        self.memcache
            .add_with_ttl(mc_key, lockstate, lock_ttl)
            .then(move |res| {
                match res {
                    Ok(true) => STATS::lease_claim.add_value(1),
                    Ok(false) => STATS::lease_conflict.add_value(1),
                    Err(_) => STATS::lease_claim_err.add_value(1),
                }
                res
            })
            .boxify()
    }

    fn wait_for_other_leases(&self, _key: &str) -> BoxFuture<(), ()> {
        let retry_millis = 200;
        let retry_delay = Duration::from_millis(retry_millis);
        STATS::lease_wait_ms.add_value(retry_millis as i64);
        self.timer.sleep(retry_delay).map_err(|_| ()).boxify()
    }

    fn release_lease(&self, key: &str, put_success: bool) -> BoxFuture<(), ()> {
        let mc_key = self.presence_keygen.key(key);
        STATS::lease_release.add_value(1);
        if put_success {
            let uploaded = compact_protocol::serialize(&LockState::uploaded_key(key.to_string()));
            STATS::lease_release_good.add_value(1);

            self.memcache.set(mc_key, uploaded).boxify()
        } else {
            STATS::lease_release_bad.add_value(1);
            self.memcache.del(mc_key).boxify()
        }
    }
}
