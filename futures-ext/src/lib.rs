// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]
#![feature(never_type)]

#[cfg(test)]
#[macro_use]
extern crate assert_matches;
#[cfg(test)]
extern crate async_unit;
extern crate bytes;
#[macro_use]
extern crate futures;
#[cfg(test)]
#[macro_use]
extern crate quickcheck;
extern crate tokio_core;
extern crate tokio_io;

use bytes::Bytes;
use futures::{Async, Future, IntoFuture, Poll, Sink, Stream};
use futures::sync::oneshot;
use tokio_io::codec::{Decoder, Encoder};

mod bytes_stream;
mod futures_ordered;
mod select_all;
mod streamfork;
mod stream_wrappers;

pub mod decode;
pub mod encode;

pub mod io;

pub use bytes_stream::{BytesStream, BytesStreamFuture};
pub use futures_ordered::{futures_ordered, FuturesOrdered};
pub use select_all::select_all;
pub use stream_wrappers::{BoxStreamWrapper, CollectNoConsume, StreamWrapper, TakeWhile};

/// Map `Item` and `Error` to `()`
///
/// Adapt an existing `Future` to return unit `Item` and `Error`, while still
/// waiting for the underlying `Future` to complete.
pub struct Discard<F>(F);

impl<F> Discard<F> {
    pub fn new(f: F) -> Self {
        Discard(f)
    }
}

impl<F> Future for Discard<F>
where
    F: Future,
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self.0.poll() {
            Err(_) => Err(()),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Ok(Async::Ready(_)) => Ok(Async::Ready(())),
        }
    }
}

// Replacements for BoxFuture and BoxStream, deprecated in upstream futures-rs.
pub type BoxFuture<T, E> = Box<Future<Item = T, Error = E> + Send>;
pub type BoxFutureNonSend<T, E> = Box<Future<Item = T, Error = E>>;
pub type BoxStream<T, E> = Box<Stream<Item = T, Error = E> + Send>;
pub type BoxStreamNonSend<T, E> = Box<Stream<Item = T, Error = E>>;

pub trait FutureExt: Future + Sized {
    /// Map a `Future` to have `Item=()` and `Error=()`. This is
    /// useful when a future is being used to drive a computation
    /// but the actual results aren't interesting (such as when used
    /// with `Handle::spawn()`).
    fn discard(self) -> Discard<Self> {
        Discard(self)
    }

    /// Create a `Send`able boxed version of this `Future`.
    #[inline]
    fn boxify(self) -> BoxFuture<Self::Item, Self::Error>
    where
        Self: 'static + Send,
    {
        // TODO: (sid0) T21801845 rename to 'boxed' once gone from upstream.
        Box::new(self)
    }

    /// Create a non-`Send`able boxed version of this `Future`.
    #[inline]
    fn boxify_nonsend(self) -> BoxFutureNonSend<Self::Item, Self::Error>
    where
        Self: 'static,
    {
        Box::new(self)
    }
}

impl<T> FutureExt for T
where
    T: Future,
{
}

pub trait StreamExt: Stream {
    /// Fork elements in a stream out to two sinks, depending on a predicate
    ///
    /// If the predicate returns false, send the item to `out1`, otherwise to
    /// `out2`. `streamfork()` acts in a similar manner to `forward()` in that it
    /// keeps operating until the input stream ends, and then returns everything
    /// in the resulting Future.
    ///
    /// The predicate returns a `Result` so that it can fail (if there's a malformed
    /// input that can't be assigned to either output).
    fn streamfork<Out1, Out2, F, E>(
        self,
        out1: Out1,
        out2: Out2,
        pred: F,
    ) -> streamfork::Forker<Self, Out1, Out2, F>
    where
        Self: Sized,
        Out1: Sink<SinkItem = Self::Item>,
        Out2: Sink<SinkItem = Self::Item, SinkError = Out1::SinkError>,
        F: FnMut(&Self::Item) -> Result<bool, E>,
        E: From<Self::Error> + From<Out1::SinkError> + From<Out2::SinkError>,
    {
        streamfork::streamfork(self, out1, out2, pred)
    }

    fn take_while_wrapper<P, R>(self, pred: P) -> TakeWhile<Self, P, R>
    where
        P: FnMut(&Self::Item) -> R,
        R: IntoFuture<Item = bool, Error = Self::Error>,
        Self: Sized,
    {
        stream_wrappers::take_while::new(self, pred)
    }

    fn collect_no_consume(self) -> CollectNoConsume<Self>
    where
        Self: Sized,
    {
        stream_wrappers::collect_no_consume::new(self)
    }

    fn encode<Enc>(self, encoder: Enc) -> encode::LayeredEncoder<Self, Enc>
    where
        Self: Sized,
        Enc: Encoder<Item = Self::Item>,
    {
        encode::encode(self, encoder)
    }

    fn enumerate(self) -> Enumerate<Self>
    where
        Self: Sized,
    {
        Enumerate::new(self)
    }

    /// Creates a stream wrapper and a future. The future will resolve into the wrapped stream when
    /// the stream wrapper returns None. It uses ConservativeReceiver to ensure that deadlocks are
    /// easily caught when one tries to poll on the receiver before consuming the stream.
    fn return_remainder(self) -> (ReturnRemainder<Self>, ConservativeReceiver<Self>)
    where
        Self: Sized,
    {
        ReturnRemainder::new(self)
    }

    /// Create a `Send`able boxed version of this `Stream`.
    #[inline]
    fn boxify(self) -> BoxStream<Self::Item, Self::Error>
    where
        Self: 'static + Send + Sized,
    {
        // TODO: (sid0) T21801845 rename to 'boxed' once gone from upstream.
        Box::new(self)
    }

    /// Create a non-`Send`able boxed version of this `Stream`.
    #[inline]
    fn boxify_nonsend(self) -> BoxStreamNonSend<Self::Item, Self::Error>
    where
        Self: 'static + Sized,
    {
        Box::new(self)
    }
}

impl<T> StreamExt for T
where
    T: Stream,
{
}

pub trait StreamLayeredExt: Stream<Item = Bytes> {
    fn decode<Dec>(self, decoder: Dec) -> decode::LayeredDecode<Self, Dec>
    where
        Self: Sized,
        Dec: Decoder;
}

impl<T> StreamLayeredExt for T
where
    T: Stream<Item = Bytes>,
{
    fn decode<Dec>(self, decoder: Dec) -> decode::LayeredDecode<Self, Dec>
    where
        Self: Sized,
        Dec: Decoder,
    {
        decode::decode(self, decoder)
    }
}

pub struct Enumerate<In> {
    inner: In,
    count: usize,
}

impl<In> Enumerate<In> {
    fn new(inner: In) -> Self {
        Enumerate {
            inner: inner,
            count: 0,
        }
    }
}

impl<In: Stream> Stream for Enumerate<In> {
    type Item = (usize, In::Item);
    type Error = In::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.inner.poll() {
            Err(err) => Err(err),
            Ok(Async::NotReady) => Ok(Async::NotReady),
            Ok(Async::Ready(None)) => Ok(Async::Ready(None)),
            Ok(Async::Ready(Some(v))) => {
                let c = self.count;
                self.count += 1;
                Ok(Async::Ready(Some((c, v))))
            }
        }
    }
}

/// This is a wrapper around oneshot::Receiver that will return error when the receiver was polled
/// and the result was not ready. This is a very strict way of preventing deadlocks in code when
/// receiver is polled before the sender has send the result
pub struct ConservativeReceiver<T>(oneshot::Receiver<T>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConservativeReceiverError {
    Canceled,
    ReceiveBeforeSend,
}

impl ::std::error::Error for ConservativeReceiverError {
    fn description(&self) -> &str {
        match self {
            &ConservativeReceiverError::Canceled => "oneshot canceled",
            &ConservativeReceiverError::ReceiveBeforeSend => "recv called on channel before send",
        }
    }
}

impl ::std::fmt::Display for ConservativeReceiverError {
    fn fmt(&self, fmt: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        match self {
            &ConservativeReceiverError::Canceled => write!(fmt, "oneshot canceled"),
            &ConservativeReceiverError::ReceiveBeforeSend => {
                write!(fmt, "recv called on channel before send")
            }
        }
    }
}

impl ::std::convert::From<oneshot::Canceled> for ConservativeReceiverError {
    fn from(_: oneshot::Canceled) -> ConservativeReceiverError {
        ConservativeReceiverError::Canceled
    }
}

impl<T> ConservativeReceiver<T> {
    pub fn new(recv: oneshot::Receiver<T>) -> Self {
        ConservativeReceiver(recv)
    }
}

impl<T> Future for ConservativeReceiver<T> {
    type Item = T;
    type Error = ConservativeReceiverError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self.0.poll()? {
            Async::Ready(item) => Ok(Async::Ready(item)),
            Async::NotReady => Err(ConservativeReceiverError::ReceiveBeforeSend),
        }
    }
}

pub struct ReturnRemainder<In> {
    inner: Option<In>,
    send: Option<oneshot::Sender<In>>,
}

impl<In> ReturnRemainder<In> {
    fn new(inner: In) -> (Self, ConservativeReceiver<In>) {
        let (send, recv) = oneshot::channel();
        (
            Self {
                inner: Some(inner),
                send: Some(send),
            },
            ConservativeReceiver::new(recv),
        )
    }
}

impl<In: Stream> Stream for ReturnRemainder<In> {
    type Item = In::Item;
    type Error = In::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let maybe_item = match self.inner {
            Some(ref mut inner) => try_ready!(inner.poll()),
            None => return Ok(Async::Ready(None)),
        };

        if maybe_item.is_none() {
            let inner = self.inner
                .take()
                .expect("inner was just polled, should be some");
            let send = self.send.take().expect("send is None iff inner is None");
            // The Receiver will handle errors
            let _ = send.send(inner);
        }

        Ok(Async::Ready(maybe_item))
    }
}

/// A convenience macro for working with `io::Result<T>` from the `Read` and
/// `Write` traits.
///
/// This macro takes `io::Result<T>` as input, and returns `Poll<T, io::Error>`
/// as the output. If the input type is of the `Err` variant, then
/// `Poll::NotReady` is returned if it indicates `WouldBlock` or otherwise `Err`
/// is returned.
#[macro_export]
macro_rules! handle_nb {
    ($e:expr) => (match $e {
        Ok(t) => Ok(::futures::Async::Ready(t)),
        Err(ref e) if e.kind() == ::std::io::ErrorKind::WouldBlock => {
            Ok(::futures::Async::NotReady)
        }
        Err(e) => Err(e),
    })
}

/// Macro that can be used like `?` operator, but in the context where the expected return type is
/// BoxFuture. The result of it is either Ok part of Result or immediate returning the Err part
/// converted into BoxFuture.
#[macro_export]
macro_rules! try_boxfuture {
    ($e:expr) => (match $e {
        Ok(t) => t,
        Err(e) => return ::futures::future::err(e.into()).boxify(),
    })
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::Stream;
    use futures::stream;
    use futures::sync::mpsc;
    use tokio_core::reactor::Core;

    #[derive(Debug)]
    struct MyErr;

    impl<T> From<mpsc::SendError<T>> for MyErr {
        fn from(_: mpsc::SendError<T>) -> Self {
            MyErr
        }
    }

    #[test]
    fn discard() {
        use futures::sync::mpsc;

        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let (tx, rx) = mpsc::channel(1);

        let xfer = stream::iter_ok::<_, MyErr>(vec![123]).forward(tx);

        handle.spawn(xfer.discard());

        match core.run(rx.collect()) {
            Ok(v) => assert_eq!(v, vec![123]),
            bad => panic!("bad {:?}", bad),
        }
    }

    #[test]
    fn enumerate() {
        let s = stream::iter_ok::<_, ()>(vec!["hello", "there", "world"]);
        let es = Enumerate::new(s);
        let v = es.collect().wait();

        assert_eq!(v, Ok(vec![(0, "hello"), (1, "there"), (2, "world")]));
    }

    #[test]
    fn return_remainder() {
        use futures::future::poll_fn;

        let s = stream::iter_ok::<_, ()>(vec!["hello", "there", "world"]).fuse();
        let (mut s, mut remainder) = s.return_remainder();

        let mut core = Core::new().unwrap();
        let res: Result<(), ()> = core.run(poll_fn(move || {
            assert_matches!(
                remainder.poll(),
                Err(ConservativeReceiverError::ReceiveBeforeSend)
            );

            assert_eq!(s.poll(), Ok(Async::Ready(Some("hello"))));
            assert_matches!(
                remainder.poll(),
                Err(ConservativeReceiverError::ReceiveBeforeSend)
            );

            assert_eq!(s.poll(), Ok(Async::Ready(Some("there"))));
            assert_matches!(
                remainder.poll(),
                Err(ConservativeReceiverError::ReceiveBeforeSend)
            );

            assert_eq!(s.poll(), Ok(Async::Ready(Some("world"))));
            assert_matches!(
                remainder.poll(),
                Err(ConservativeReceiverError::ReceiveBeforeSend)
            );

            assert_eq!(s.poll(), Ok(Async::Ready(None)));
            match remainder.poll() {
                Ok(Async::Ready(s)) => assert!(s.is_done()),
                bad => panic!("unexpected result: {:?}", bad),
            }

            Ok(Async::Ready(()))
        }));

        assert_matches!(res, Ok(()));
    }
}
