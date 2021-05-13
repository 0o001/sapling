/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use crate::base::{MultiplexedBlobstoreBase, MultiplexedBlobstorePutHandler};
use crate::queue::MultiplexedBlobstore;
use crate::scrub::{LoggingScrubHandler, ScrubAction, ScrubBlobstore, ScrubHandler, ScrubOptions};
use anyhow::{bail, Result};
use async_trait::async_trait;
use blobstore::{Blobstore, BlobstoreGetData, BlobstorePutOps, OverwriteStatus, PutBehaviour};
use blobstore_sync_queue::{
    BlobstoreSyncQueue, BlobstoreSyncQueueEntry, OperationKey, SqlBlobstoreSyncQueue,
};
use borrowed::borrowed;
use bytes::Bytes;
use cloned::cloned;
use context::{CoreContext, SessionClass, SessionContainer};
use fbinit::FacebookInit;
use futures::{
    channel::oneshot,
    future::{FutureExt, TryFutureExt},
    task::{Context, Poll},
};
use lock_ext::LockExt;
use memblob::Memblob;
use metaconfig_types::{BlobstoreId, MultiplexId};
use mononoke_types::{BlobstoreBytes, DateTime};
use nonzero_ext::nonzero;
use readonlyblob::ReadOnlyBlobstore;
use scuba_ext::MononokeScubaSampleBuilder;
use sql_construct::SqlConstruct;

pub struct Tickable<T> {
    pub storage: Arc<Mutex<HashMap<String, T>>>,
    // queue of pending operations
    queue: Arc<Mutex<VecDeque<oneshot::Sender<Option<String>>>>>,
}

impl<T: fmt::Debug> fmt::Debug for Tickable<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tickable")
            .field("storage", &self.storage)
            .field("pending", &self.queue.with(|q| q.len()))
            .finish()
    }
}

impl<T> fmt::Display for Tickable<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Tickable")
    }
}

impl<T> Tickable<T> {
    pub fn new() -> Self {
        Self {
            storage: Default::default(),
            queue: Default::default(),
        }
    }

    pub fn add_content(&self, key: String, value: T) {
        self.storage.with(|s| {
            s.insert(key, value);
        })
    }

    // Broadcast either success or error to a set of outstanding futures, advancing the
    // overall state by one tick.
    pub fn tick(&self, error: Option<&str>) {
        let mut queue = self.queue.lock().unwrap();
        for send in queue.drain(..) {
            send.send(error.map(String::from)).unwrap();
        }
    }

    // Register this task on the tick queue and wait for it to progress.

    pub fn on_tick(&self) -> impl Future<Output = Result<()>> {
        let (send, recv) = oneshot::channel();
        let mut queue = self.queue.lock().unwrap();
        queue.push_back(send);
        async move {
            let error = recv.await?;
            match error {
                None => Ok(()),
                Some(error) => bail!(error),
            }
        }
    }
}

#[async_trait]
impl Blobstore for Tickable<BlobstoreBytes> {
    async fn get<'a>(
        &'a self,
        _ctx: &'a CoreContext,
        key: &'a str,
    ) -> Result<Option<BlobstoreGetData>> {
        let storage = self.storage.clone();
        let on_tick = self.on_tick();

        on_tick.await?;
        Ok(storage.with(|s| s.get(key).cloned().map(Into::into)))
    }

    async fn put<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<()> {
        BlobstorePutOps::put_with_status(self, ctx, key, value).await?;
        Ok(())
    }
}

#[async_trait]
impl BlobstorePutOps for Tickable<BlobstoreBytes> {
    async fn put_explicit<'a>(
        &'a self,
        _ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
        _put_behaviour: PutBehaviour,
    ) -> Result<OverwriteStatus> {
        let storage = self.storage.clone();
        let on_tick = self.on_tick();

        on_tick.await?;
        storage.with(|s| {
            s.insert(key, value);
        });
        Ok(OverwriteStatus::NotChecked)
    }

    async fn put_with_status<'a>(
        &'a self,
        ctx: &'a CoreContext,
        key: String,
        value: BlobstoreBytes,
    ) -> Result<OverwriteStatus> {
        self.put_explicit(ctx, key, value, PutBehaviour::Overwrite)
            .await
    }
}

#[async_trait]
impl MultiplexedBlobstorePutHandler for Tickable<BlobstoreId> {
    async fn on_put<'out>(
        &'out self,
        _ctx: &'out CoreContext,
        blobstore_id: BlobstoreId,
        _multiplex_id: MultiplexId,
        _operation_key: &'out OperationKey,
        key: &'out str,
        _blob_size: Option<u64>,
    ) -> Result<()> {
        let storage = self.storage.clone();
        let key = key.to_string();
        self.on_tick().await?;
        storage.with(|s| {
            s.insert(key, blobstore_id);
        });
        Ok(())
    }
}

struct LogHandler {
    pub log: Arc<Mutex<Vec<(BlobstoreId, String)>>>,
}

impl LogHandler {
    fn new() -> Self {
        Self {
            log: Default::default(),
        }
    }
    fn clear(&self) {
        self.log.with(|log| log.clear())
    }
}

#[async_trait]
impl MultiplexedBlobstorePutHandler for LogHandler {
    async fn on_put<'out>(
        &'out self,
        _ctx: &'out CoreContext,
        blobstore_id: BlobstoreId,
        _multiplex_id: MultiplexId,
        _operation_key: &'out OperationKey,
        key: &'out str,
        _blob_size: Option<u64>,
    ) -> Result<()> {
        self.log
            .with(move |log| log.push((blobstore_id, key.to_string())));
        Ok(())
    }
}

fn make_value(value: &str) -> BlobstoreBytes {
    BlobstoreBytes::from_bytes(Bytes::copy_from_slice(value.as_bytes()))
}

struct PollOnce<'a, F> {
    future: Pin<&'a mut F>,
}

impl<'a, F> PollOnce<'a, F> {
    pub fn new(future: Pin<&'a mut F>) -> Self {
        Self { future }
    }
}

impl<'a, F: Future + Unpin> Future for PollOnce<'a, F> {
    type Output = Poll<<F as Future>::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // This is pin-projection; I uphold the Pin guarantees, so it's fine.
        let this = unsafe { self.get_unchecked_mut() };
        Poll::Ready(this.future.poll_unpin(cx))
    }
}

#[fbinit::test]
async fn scrub_blobstore_fetch_none(fb: FacebookInit) -> Result<()> {
    let bid0 = BlobstoreId::new(0);
    let bs0 = Arc::new(Tickable::new());
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Tickable::new());
    let bid2 = BlobstoreId::new(2);
    let bs2 = Arc::new(Tickable::new());

    let queue = Arc::new(SqlBlobstoreSyncQueue::with_sqlite_in_memory().unwrap());

    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);
    let bs = ScrubBlobstore::new(
        MultiplexId::new(1),
        vec![(bid0, bs0.clone()), (bid1, bs1.clone())],
        vec![(bid2, bs2.clone())],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
        ScrubOptions {
            scrub_action: ScrubAction::ReportOnly,
            scrub_grace: None,
        },
        Arc::new(LoggingScrubHandler::new(false)) as Arc<dyn ScrubHandler>,
    );

    let mut fut = bs.get(ctx, "key");
    assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

    // No entry for "key" - blobstores return None...
    bs0.tick(None);
    bs1.tick(None);
    // Expect a read from writemostly stores regardless
    bs2.tick(None);

    // but then somebody writes it
    let entry = BlobstoreSyncQueueEntry {
        blobstore_key: "key".to_string(),
        blobstore_id: bid0,
        multiplex_id: MultiplexId::new(1),
        timestamp: DateTime::now(),
        id: None,
        operation_key: OperationKey::gen(),
        blob_size: None,
    };
    queue.add(ctx, entry).await?;

    fut.await?;

    Ok(())
}

#[fbinit::test]
async fn base(fb: FacebookInit) {
    let bs0 = Arc::new(Tickable::new());
    let bs1 = Arc::new(Tickable::new());
    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![
            (BlobstoreId::new(0), bs0.clone()),
            (BlobstoreId::new(1), bs1.clone()),
        ],
        vec![],
        nonzero!(1usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    // succeed as soon as first blobstore succeeded
    {
        let v0 = make_value("v0");
        let k0 = "k0";

        let mut put_fut = bs
            .put(ctx, k0.to_owned(), v0.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(None);
        put_fut.await.unwrap();
        assert_eq!(bs0.storage.with(|s| s.get(k0).cloned()), Some(v0.clone()));
        assert!(bs1.storage.with(|s| s.is_empty()));
        bs1.tick(Some("bs1 failed"));
        assert!(
            log.log
                .with(|log| log == &vec![(BlobstoreId::new(0), k0.to_owned())])
        );

        // should succeed as it is stored in bs1
        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(None);
        bs1.tick(None);
        assert_eq!(get_fut.await.unwrap(), Some(v0.into()));
        assert!(bs1.storage.with(|s| s.is_empty()));

        log.clear();
    }

    // wait for second if first one failed
    {
        let v1 = make_value("v1");
        let k1 = "k1";

        let mut put_fut = bs
            .put(ctx, k1.to_owned(), v1.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(Some("case 2: bs0 failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs1.tick(None);
        put_fut.await.unwrap();
        assert!(bs0.storage.with(|s| s.get(k1).is_none()));
        assert_eq!(bs1.storage.with(|s| s.get(k1).cloned()), Some(v1.clone()));
        assert!(
            log.log
                .with(|log| log == &vec![(BlobstoreId::new(1), k1.to_owned())])
        );

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs1.tick(None);
        assert_eq!(get_fut.await.unwrap(), Some(v1.into()));
        assert!(bs0.storage.with(|s| s.get(k1).is_none()));

        log.clear();
    }

    // both fail => whole put fail
    {
        let v2 = make_value("v2");
        let k2 = "k2";

        let mut put_fut = bs
            .put(ctx, k2.to_owned(), v2.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(Some("case 3: bs0 failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs1.tick(Some("case 3: bs1 failed"));
        assert!(put_fut.await.is_err());
    }

    // get: Error + None -> Error
    {
        let k3 = "k3";
        let mut get_fut = bs.get(ctx, k3).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(Some("case 4: bs0 failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(None);
        assert!(get_fut.await.is_err());
    }

    // get: None + None -> None
    {
        let k3 = "k3";
        let mut get_fut = bs.get(ctx, k3).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(None);
        assert_eq!(get_fut.await.unwrap(), None);
    }

    // both put succeed
    {
        let v4 = make_value("v4");
        let k4 = "k4";
        log.clear();

        let mut put_fut = bs
            .put(ctx, k4.to_owned(), v4.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(None);
        put_fut.await.unwrap();
        assert_eq!(bs0.storage.with(|s| s.get(k4).cloned()), Some(v4.clone()));
        bs1.tick(None);
        while log.log.with(|log| log.len() != 2) {
            tokio::task::yield_now().await;
        }
        assert_eq!(bs1.storage.with(|s| s.get(k4).cloned()), Some(v4.clone()));
    }
}

#[fbinit::test]
async fn multiplexed(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);
    let queue = Arc::new(SqlBlobstoreSyncQueue::with_sqlite_in_memory().unwrap());

    let bid0 = BlobstoreId::new(0);
    let bs0 = Arc::new(Tickable::new());
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Tickable::new());
    let bs = MultiplexedBlobstore::new(
        MultiplexId::new(1),
        vec![(bid0, bs0.clone()), (bid1, bs1.clone())],
        vec![],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    // non-existing key when one blobstore failing
    {
        let k0 = "k0";

        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(Some("case 1: bs1 failed"));
        assert_eq!(get_fut.await.unwrap(), None);
    }

    // only replica containing key failed
    {
        let v1 = make_value("v1");
        let k1 = "k1";

        let mut put_fut = bs
            .put(ctx, k1.to_owned(), v1.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(None);
        bs1.tick(Some("case 2: bs1 failed"));
        put_fut.await.expect("case 2 put_fut failed");

        match queue
            .get(ctx, k1)
            .await
            .expect("case 2 get failed")
            .as_slice()
        {
            [entry] => assert_eq!(entry.blobstore_id, bid0),
            _ => panic!("only one entry expected"),
        }

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(Some("case 2: bs0 failed"));
        bs1.tick(None);
        assert!(get_fut.await.is_err());
    }

    // both replicas fail
    {
        let k2 = "k2";

        let mut get_fut = bs.get(ctx, k2).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(Some("case 3: bs0 failed"));
        bs1.tick(Some("case 3: bs1 failed"));
        assert!(get_fut.await.is_err());
    }
}

#[fbinit::test]
async fn multiplexed_operation_keys(fb: FacebookInit) -> Result<()> {
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);
    let queue = Arc::new(SqlBlobstoreSyncQueue::with_sqlite_in_memory().unwrap());

    let bid0 = BlobstoreId::new(0);
    let bs0 = Arc::new(Memblob::default());
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Memblob::default());
    let bid2 = BlobstoreId::new(2);
    // we need writes to fail there so there's something on the queue
    let bs2 = Arc::new(ReadOnlyBlobstore::new(Memblob::default()));
    let bs = MultiplexedBlobstore::new(
        MultiplexId::new(1),
        vec![
            (bid0, bs0.clone()),
            (bid1, bs1.clone()),
            (bid2, bs2.clone()),
        ],
        vec![],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    // two replicas succeed, one fails the operation keys are equal and non-null
    {
        let v3 = make_value("v3");
        let k3 = "k3";

        bs.put(ctx, k3.to_owned(), v3.clone())
            .map_err(|_| ())
            .await
            .expect("test multiplexed_operation_keys, put failed");

        match queue
            .get(ctx, k3)
            .await
            .expect("test multiplexed_operation_keys, get failed")
            .as_slice()
        {
            [entry0, entry1] => {
                assert_eq!(entry0.operation_key, entry1.operation_key);
                assert!(!entry0.operation_key.is_null());
            }
            x => panic!("two entries expected, got {:?}", x),
        }
    }
    Ok(())
}

#[fbinit::test]
async fn multiplexed_blob_size(fb: FacebookInit) -> Result<()> {
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);
    let queue = Arc::new(SqlBlobstoreSyncQueue::with_sqlite_in_memory().unwrap());

    let bid0 = BlobstoreId::new(0);
    let bs0 = Arc::new(Memblob::default());
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Memblob::default());
    let bid2 = BlobstoreId::new(2);
    // we need writes to fail there so there's something on the queue
    let bs2 = Arc::new(ReadOnlyBlobstore::new(Memblob::default()));
    let bs = MultiplexedBlobstore::new(
        MultiplexId::new(1),
        vec![
            (bid0, bs0.clone()),
            (bid1, bs1.clone()),
            (bid2, bs2.clone()),
        ],
        vec![],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    // two replicas succeed, one fails blob sizes are correct
    {
        let key = "key";
        let value = make_value("value");

        bs.put(ctx, key.to_owned(), value.clone()).await?;

        match queue.get(ctx, key).await?.as_slice() {
            [entry0, entry1] => {
                assert_eq!(
                    entry0.blob_size.expect("blob size is None"),
                    value.len() as u64
                );
                assert_eq!(
                    entry1.blob_size.expect("blob size is None"),
                    value.len() as u64
                );
            }
            x => panic!("two entries expected, got {:?}", x),
        }
    }
    Ok(())
}

#[fbinit::test]
async fn scrubbed(fb: FacebookInit) {
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);
    let queue = Arc::new(SqlBlobstoreSyncQueue::with_sqlite_in_memory().unwrap());
    let scrub_handler = Arc::new(LoggingScrubHandler::new(false)) as Arc<dyn ScrubHandler>;
    let bid0 = BlobstoreId::new(0);
    let bs0 = Arc::new(Tickable::new());
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Tickable::new());
    let bid2 = BlobstoreId::new(2);
    let bs2 = Arc::new(Tickable::new());
    let bs = ScrubBlobstore::new(
        MultiplexId::new(1),
        vec![(bid0, bs0.clone()), (bid1, bs1.clone())],
        vec![(bid2, bs2.clone())],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
        ScrubOptions {
            scrub_action: ScrubAction::ReportOnly,
            scrub_grace: None,
        },
        scrub_handler.clone(),
    );

    // non-existing key when one main blobstore failing
    {
        let k0 = "k0";

        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(Some("bs1 failed"));

        bs2.tick(None);
        assert_eq!(get_fut.await.unwrap(), None, "SomeNone + Err expected None");
    }

    // non-existing key when one write mostly blobstore failing
    {
        let k0 = "k0";

        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(None);

        bs2.tick(Some("bs1 failed"));
        assert_eq!(get_fut.await.unwrap(), None, "SomeNone + Err expected None");
    }

    // fail all but one store on put to make sure only one has the data
    // only replica containing key fails on read.
    {
        let v1 = make_value("v1");
        let k1 = "k1";

        let mut put_fut = bs
            .put(ctx, k1.to_owned(), v1.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        bs1.tick(Some("bs1 failed"));
        bs2.tick(Some("bs2 failed"));
        put_fut.await.unwrap();

        match queue.get(ctx, k1).await.unwrap().as_slice() {
            [entry] => {
                assert_eq!(entry.blobstore_id, bid0, "Queue bad");
            }
            _ => panic!("only one entry expected"),
        }

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(Some("bs0 failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(None);
        bs2.tick(None);
        assert!(get_fut.await.is_err(), "None/Err while replicating");
    }

    // all replicas fail
    {
        let k2 = "k2";

        let mut get_fut = bs.get(ctx, k2).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(Some("bs0 failed"));
        bs1.tick(Some("bs1 failed"));
        bs2.tick(Some("bs1 failed"));
        assert!(get_fut.await.is_err(), "Err/Err");
    }

    // Now replace bs1 & bs2 with an empty blobstore, and see the scrub work
    let bid1 = BlobstoreId::new(1);
    let bs1 = Arc::new(Tickable::new());
    let bid2 = BlobstoreId::new(2);
    let bs2 = Arc::new(Tickable::new());
    let bs = ScrubBlobstore::new(
        MultiplexId::new(1),
        vec![(bid0, bs0.clone()), (bid1, bs1.clone())],
        vec![(bid2, bs2.clone())],
        nonzero!(1usize),
        queue.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
        ScrubOptions {
            scrub_action: ScrubAction::Repair,
            scrub_grace: None,
        },
        scrub_handler,
    );

    // Non-existing key in all blobstores, new blobstore failing
    {
        let k0 = "k0";

        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);

        bs1.tick(Some("bs1 failed"));

        bs2.tick(None);
        assert_eq!(get_fut.await.unwrap(), None, "None/Err after replacement");
    }

    // only replica containing key replaced after failure - DATA LOST
    {
        let k1 = "k1";

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs0.tick(Some("bs0 failed"));
        bs1.tick(None);
        bs2.tick(None);
        assert!(get_fut.await.is_err(), "Empty replacement against error");
    }

    // One working replica after failure.
    {
        let v1 = make_value("v1");
        let k1 = "k1";

        match queue.get(ctx, k1).await.unwrap().as_slice() {
            [entry] => {
                assert_eq!(entry.blobstore_id, bid0, "Queue bad");
                queue
                    .del(&ctx, &vec![entry.clone()])
                    .await
                    .expect("Could not delete scrub queue entry");
            }
            _ => panic!("only one entry expected"),
        }

        // bs1 empty at this point
        assert_eq!(bs0.storage.with(|s| s.get(k1).cloned()), Some(v1.clone()));
        assert!(bs1.storage.with(|s| s.is_empty()));

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        // tick the gets
        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        bs1.tick(None);
        bs2.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        // Tick the repairs
        bs1.tick(None);
        bs2.tick(None);

        // Succeeds
        assert_eq!(get_fut.await.unwrap(), Some(v1.clone().into()));
        // Now all populated.
        assert_eq!(bs0.storage.with(|s| s.get(k1).cloned()), Some(v1.clone()));
        assert_eq!(bs1.storage.with(|s| s.get(k1).cloned()), Some(v1.clone()));
        assert_eq!(bs2.storage.with(|s| s.get(k1).cloned()), Some(v1.clone()));
    }
}

#[fbinit::test]
async fn queue_waits(fb: FacebookInit) {
    let bs0 = Arc::new(Tickable::new());
    let bs1 = Arc::new(Tickable::new());
    let bs2 = Arc::new(Tickable::new());
    let log = Arc::new(Tickable::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![
            (BlobstoreId::new(0), bs0.clone()),
            (BlobstoreId::new(1), bs1.clone()),
            (BlobstoreId::new(2), bs2.clone()),
        ],
        vec![],
        nonzero!(1usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );
    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    let clear = {
        cloned!(bs0, bs1, bs2, log);
        move || {
            bs0.tick(None);
            bs1.tick(None);
            bs2.tick(None);
            log.tick(None);
        }
    };

    let v = make_value("v");
    let k = "k";

    // Put succeeds once all blobstores have succeded, even if the queue hasn't.
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        bs1.tick(None);
        bs2.tick(None);

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Ready(Ok(())));

        clear();
    }

    // Put succeeds after 1 write + a write to the queue
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        log.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Ready(Ok(())));

        clear();
    }

    // Put succeeds despite errors, if the queue succeeds
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        bs1.tick(Some("oops"));
        bs2.tick(Some("oops"));
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending); // Trigger on_put

        log.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Ready(Ok(())));

        clear();
    }

    // Put succeeds if any blobstore succeeds and writes to the queue
    {
        let mut fut = bs.put(ctx, k.to_owned(), v).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(Some("oops"));
        bs1.tick(None);
        bs2.tick(Some("oops"));
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending); // Trigger on_put

        log.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Ready(Ok(())));

        clear();
    }
}

#[fbinit::test]
async fn write_mostly_get(fb: FacebookInit) {
    let both_key = "both";
    let value = make_value("value");
    let write_mostly_key = "write_mostly";
    let main_only_key = "main_only";
    let main_bs = Arc::new(Tickable::new());
    let write_mostly_bs = Arc::new(Tickable::new());

    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![(BlobstoreId::new(0), main_bs.clone())],
        vec![(BlobstoreId::new(1), write_mostly_bs.clone())],
        nonzero!(1usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    // Put one blob into both blobstores
    main_bs.add_content(both_key.to_owned(), value.clone());
    main_bs.add_content(main_only_key.to_owned(), value.clone());
    write_mostly_bs.add_content(both_key.to_owned(), value.clone());
    // Put a blob only into the write mostly blobstore
    write_mostly_bs.add_content(write_mostly_key.to_owned(), value.clone());

    // Fetch the blob that's in both blobstores, see that the write mostly blobstore isn't being
    // read from by ticking it
    {
        let mut fut = bs.get(ctx, both_key);
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Ticking the write_mostly store does nothing.
        for _ in 0..3usize {
            write_mostly_bs.tick(None);
            assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());
        }

        // Tick the main store, and we're finished
        main_bs.tick(None);
        assert_eq!(fut.await.unwrap(), Some(value.clone().into()));
        log.clear();
    }

    // Fetch the blob that's only in the write mostly blobstore, see it fetch correctly
    {
        let mut fut = bs.get(ctx, write_mostly_key);
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Ticking the main store does nothing, as it lacks the blob
        for _ in 0..3usize {
            main_bs.tick(None);
            assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());
        }

        // Tick the write_mostly store, and we're finished
        write_mostly_bs.tick(None);
        assert_eq!(fut.await.unwrap(), Some(value.clone().into()));
        log.clear();
    }

    // Fetch the blob that's in both blobstores, see that the write mostly blobstore
    // is used when the main blobstore fails
    {
        let mut fut = bs.get(ctx, both_key);
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Ticking the write_mostly store does nothing.
        for _ in 0..3usize {
            write_mostly_bs.tick(None);
            assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());
        }

        // Tick the main store, and we're still stuck
        main_bs.tick(Some("Main blobstore failed - fallback to write_mostly"));
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Finally, the write_mostly store returns our value
        write_mostly_bs.tick(None);
        assert_eq!(fut.await.unwrap(), Some(value.clone().into()));
        log.clear();
    }

    // Fetch the blob that's in main blobstores, see that the write mostly blobstore
    // None value is not used when the main blobstore fails
    {
        let mut fut = bs.get(ctx, main_only_key);
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Ticking the write_mostly store does nothing.
        for _ in 0..3usize {
            write_mostly_bs.tick(None);
            assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());
        }

        // Tick the main store, and we're still stuck
        main_bs.tick(Some("Main blobstore failed - fallback to write_mostly"));
        assert!(PollOnce::new(Pin::new(&mut fut)).await.is_pending());

        // Finally, should get an error as None from a write mostly is indeterminate
        // as it might not have been fully populated yet
        write_mostly_bs.tick(None);
        assert_eq!(
            fut.await.err().unwrap().to_string().as_str(),
            "All blobstores failed: {BlobstoreId(0): Main blobstore failed - fallback to write_mostly}"
        );
        log.clear();
    }
}

#[fbinit::test]
async fn write_mostly_put(fb: FacebookInit) {
    let main_bs = Arc::new(Tickable::new());
    let write_mostly_bs = Arc::new(Tickable::new());

    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![(BlobstoreId::new(0), main_bs.clone())],
        vec![(BlobstoreId::new(1), write_mostly_bs.clone())],
        nonzero!(1usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    // succeed as soon as main succeeds. Fail write_mostly to confirm that we can still read.
    {
        let v0 = make_value("v0");
        let k0 = "k0";

        let mut put_fut = bs
            .put(ctx, k0.to_owned(), v0.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        main_bs.tick(None);
        put_fut.await.unwrap();
        assert_eq!(
            main_bs.storage.with(|s| s.get(k0).cloned()),
            Some(v0.clone())
        );
        assert!(write_mostly_bs.storage.with(|s| s.is_empty()));
        write_mostly_bs.tick(Some("write_mostly_bs failed"));
        assert!(
            log.log
                .with(|log| log == &vec![(BlobstoreId::new(0), k0.to_owned())])
        );

        // should succeed as it is stored in main_bs
        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        main_bs.tick(None);
        write_mostly_bs.tick(None);
        assert_eq!(get_fut.await.unwrap(), Some(v0.into()));
        assert!(write_mostly_bs.storage.with(|s| s.is_empty()));

        main_bs.storage.with(|s| s.clear());
        write_mostly_bs.storage.with(|s| s.clear());
        log.clear();
    }

    // succeed as soon as write_mostly succeeds. Fail main to confirm we can still read
    {
        let v0 = make_value("v0");
        let k0 = "k0";

        let mut put_fut = bs
            .put(ctx, k0.to_owned(), v0.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        write_mostly_bs.tick(None);
        put_fut.await.unwrap();
        assert_eq!(
            write_mostly_bs.storage.with(|s| s.get(k0).cloned()),
            Some(v0.clone())
        );
        assert!(main_bs.storage.with(|s| s.is_empty()));
        main_bs.tick(Some("main_bs failed"));
        assert!(
            log.log
                .with(|log| log == &vec![(BlobstoreId::new(1), k0.to_owned())])
        );

        // should succeed as it is stored in write_mostly_bs, but main won't read
        let mut get_fut = bs.get(ctx, k0).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        main_bs.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        write_mostly_bs.tick(None);
        assert_eq!(get_fut.await.unwrap(), Some(v0.into()));
        assert!(main_bs.storage.with(|s| s.is_empty()));

        main_bs.storage.with(|s| s.clear());
        write_mostly_bs.storage.with(|s| s.clear());
        log.clear();
    }

    // succeed if write_mostly succeeds and main fails
    {
        let v1 = make_value("v1");
        let k1 = "k1";

        let mut put_fut = bs
            .put(ctx, k1.to_owned(), v1.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        main_bs.tick(Some("case 2: main_bs failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        write_mostly_bs.tick(None);
        put_fut.await.unwrap();
        assert!(main_bs.storage.with(|s| s.get(k1).is_none()));
        assert_eq!(
            write_mostly_bs.storage.with(|s| s.get(k1).cloned()),
            Some(v1.clone())
        );
        assert!(
            log.log
                .with(|log| log == &vec![(BlobstoreId::new(1), k1.to_owned())])
        );

        let mut get_fut = bs.get(ctx, k1).map_err(|_| ()).boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        main_bs.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut get_fut)).await, Poll::Pending);
        write_mostly_bs.tick(None);
        assert_eq!(get_fut.await.unwrap(), Some(v1.into()));
        assert!(main_bs.storage.with(|s| s.get(k1).is_none()));

        main_bs.storage.with(|s| s.clear());
        write_mostly_bs.storage.with(|s| s.clear());
        log.clear();
    }

    // both fail => whole put fail
    {
        let v2 = make_value("v2");
        let k2 = "k2";

        let mut put_fut = bs
            .put(ctx, k2.to_owned(), v2.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        main_bs.tick(Some("case 3: main_bs failed"));
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        write_mostly_bs.tick(Some("case 3: write_mostly_bs failed"));
        assert!(put_fut.await.is_err());
    }

    // both put succeed
    {
        let v4 = make_value("v4");
        let k4 = "k4";
        main_bs.storage.with(|s| s.clear());
        write_mostly_bs.storage.with(|s| s.clear());
        log.clear();

        let mut put_fut = bs
            .put(ctx, k4.to_owned(), v4.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);
        main_bs.tick(None);
        put_fut.await.unwrap();
        assert_eq!(
            main_bs.storage.with(|s| s.get(k4).cloned()),
            Some(v4.clone())
        );
        write_mostly_bs.tick(None);
        while log.log.with(|log| log.len() != 2) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            write_mostly_bs.storage.with(|s| s.get(k4).cloned()),
            Some(v4.clone())
        );
    }
}

#[fbinit::test]
async fn needed_writes(fb: FacebookInit) {
    let main_bs0 = Arc::new(Tickable::new());
    let main_bs2 = Arc::new(Tickable::new());
    let write_mostly_bs = Arc::new(Tickable::new());

    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![
            (BlobstoreId::new(0), main_bs0.clone()),
            (BlobstoreId::new(2), main_bs2.clone()),
        ],
        vec![(BlobstoreId::new(1), write_mostly_bs.clone())],
        nonzero!(2usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    // Puts do not succeed until we have two successful writes and two handlers done
    {
        let v0 = make_value("v0");
        let k0 = "k0";
        let mut put_fut = bs
            .put(ctx, k0.to_owned(), v0.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);

        main_bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);

        log.log.with(|l| {
            assert_eq!(l.len(), 1, "No handler run for put to blobstore");
            assert_eq!(
                l[0],
                (BlobstoreId::new(0), k0.to_owned()),
                "Handler put wrong entries"
            )
        });

        main_bs2.tick(None);
        assert!(
            put_fut.await.is_ok(),
            "Put failed with two succcessful writes"
        );
        log.log.with(|l| {
            assert_eq!(l.len(), 2, "No handler run for put to blobstore");
            assert_eq!(
                l,
                &vec![
                    (BlobstoreId::new(0), k0.to_owned()),
                    (BlobstoreId::new(2), k0.to_owned()),
                ],
                "Handler put wrong entries"
            )
        });

        assert_eq!(
            main_bs0.storage.with(|s| s.get(k0).cloned()),
            Some(v0.clone())
        );
        assert_eq!(
            main_bs2.storage.with(|s| s.get(k0).cloned()),
            Some(v0.clone())
        );
        assert_eq!(write_mostly_bs.storage.with(|s| s.get(k0).cloned()), None);
        write_mostly_bs.tick(Some("Error"));
        log.clear();
    }

    // A write-mostly counts as a success.
    {
        let v1 = make_value("v1");
        let k1 = "k1";
        let mut put_fut = bs
            .put(ctx, k1.to_owned(), v1.clone())
            .map_err(|_| ())
            .boxed();
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);

        main_bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut put_fut)).await, Poll::Pending);

        log.log.with(|l| {
            assert_eq!(l.len(), 1, "No handler run for put to blobstore");
            assert_eq!(
                l[0],
                (BlobstoreId::new(0), k1.to_owned()),
                "Handler put wrong entries"
            )
        });

        write_mostly_bs.tick(None);
        assert!(
            put_fut.await.is_ok(),
            "Put failed with two succcessful writes"
        );
        log.log.with(|l| {
            assert_eq!(l.len(), 2, "No handler run for put to blobstore");
            assert_eq!(
                l,
                &vec![
                    (BlobstoreId::new(0), k1.to_owned()),
                    (BlobstoreId::new(1), k1.to_owned()),
                ],
                "Handler put wrong entries"
            )
        });

        assert_eq!(
            main_bs0.storage.with(|s| s.get(k1).cloned()),
            Some(v1.clone())
        );
        assert_eq!(
            write_mostly_bs.storage.with(|s| s.get(k1).cloned()),
            Some(v1.clone())
        );
        assert_eq!(main_bs2.storage.with(|s| s.get(k1).cloned()), None);
        main_bs2.tick(Some("Error"));
        log.clear();
    }
}

#[fbinit::test]
async fn needed_writes_bad_config(fb: FacebookInit) {
    let main_bs0 = Arc::new(Tickable::new());
    let main_bs2 = Arc::new(Tickable::new());
    let write_mostly_bs = Arc::new(Tickable::new());

    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![
            (BlobstoreId::new(0), main_bs0.clone()),
            (BlobstoreId::new(2), main_bs2.clone()),
        ],
        vec![(BlobstoreId::new(1), write_mostly_bs.clone())],
        nonzero!(5usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );

    let ctx = CoreContext::test_mock(fb);
    borrowed!(ctx);

    {
        let v0 = make_value("v0");
        let k0 = "k0";
        let put_fut = bs
            .put(ctx, k0.to_owned(), v0.clone())
            .map_err(|_| ())
            .boxed();

        main_bs0.tick(None);
        main_bs2.tick(None);
        write_mostly_bs.tick(None);

        assert!(
            put_fut.await.is_err(),
            "Put succeeded despite not enough blobstores"
        );
        log.clear();
    }
}

#[fbinit::test]
async fn no_handlers(fb: FacebookInit) {
    let bs0 = Arc::new(Tickable::new());
    let bs1 = Arc::new(Tickable::new());
    let bs2 = Arc::new(Tickable::new());
    let log = Arc::new(LogHandler::new());
    let bs = MultiplexedBlobstoreBase::new(
        MultiplexId::new(1),
        vec![
            (BlobstoreId::new(0), bs0.clone()),
            (BlobstoreId::new(1), bs1.clone()),
            (BlobstoreId::new(2), bs2.clone()),
        ],
        vec![],
        nonzero!(1usize),
        log.clone(),
        MononokeScubaSampleBuilder::with_discard(),
        nonzero!(1u64),
    );
    let ctx_session = SessionContainer::builder(fb)
        .session_class(SessionClass::Background)
        .build();
    let ctx = CoreContext::test_mock_session(ctx_session);
    borrowed!(ctx);

    let clear = {
        cloned!(bs0, bs1, bs2, log);
        move || {
            bs0.tick(None);
            bs1.tick(None);
            bs2.tick(None);
            log.clear();
        }
    };

    let k = String::from("k");
    let v = make_value("v");

    // Put succeeds once all blobstores have succeded. The handlers won't run
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        bs1.tick(None);
        bs2.tick(None);
        fut.await.expect("Put should have succeeded");

        log.log
            .with(|l| assert!(l.is_empty(), "Handlers ran, yet all blobstores succeeded"));
        clear();
    }

    // Put is still in progress after one write, because no handlers have run
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);
        log.log
            .with(|l| assert!(l.is_empty(), "Handlers ran, yet put in progress"));

        bs1.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);
        log.log
            .with(|l| assert!(l.is_empty(), "Handlers ran, yet put in progress"));

        bs2.tick(None);
        fut.await.expect("Put should have succeeded");
        log.log
            .with(|l| assert!(l.is_empty(), "Handlers ran, yet all blobstores succeeded"));

        clear();
    }

    // Put succeeds despite errors, if the queue succeeds
    {
        let mut fut = bs.put(ctx, k.to_owned(), v.clone()).map_err(|_| ()).boxed();

        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);

        bs0.tick(None);
        assert_eq!(PollOnce::new(Pin::new(&mut fut)).await, Poll::Pending);
        bs1.tick(Some("oops"));
        fut.await.expect("Put should have succeeded");

        log.log.with(|l| {
            assert!(
                l.len() == 1,
                "Handlers did not run after a blobstore failure"
            )
        });
        bs2.tick(None);
        // Yield to let the spawned puts and handlers run
        tokio::task::yield_now().await;

        log.log.with(|l| {
            assert!(
                l.len() == 2,
                "Handlers did not run for both successful blobstores"
            )
        });

        clear();
    }
}
