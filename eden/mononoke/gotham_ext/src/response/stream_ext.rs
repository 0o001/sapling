/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::pin::Pin;

use futures::{
    prelude::*,
    ready,
    task::{Context, Poll},
};
use pin_project::pin_project;

use super::error_meta::{ErrorMeta, ErrorMetaProvider};

pub trait ResponseTryStreamExt: TryStream {
    /// Filter out errors from a `TryStream` and forward them into the given
    /// `Sink`, transforming the `TryStream` into a `Stream<Item=Self::Ok>`.
    ///
    /// Note that the `Stream` will wait until the `Sink` has accepted each
    /// error before advancing to the next item, so if the `Sink` fills up,
    /// the `Stream` will not be polled again until the `Sink` is ready to
    /// accept more items.
    fn forward_err<S: Sink<Self::Error>>(self, sink: S) -> ForwardErr<Self, S, Self::Error>
    where
        Self: Sized,
    {
        ForwardErr::new(self, sink)
    }

    /// Immediately end the `TryStream` upon encountering an error.
    ///
    /// The error will be passed to the given callback, and the stream will be
    /// fused to prevent the underlying `TryStream` from being polled again.
    fn end_on_err<E>(self) -> EndOnErr<Self, E>
    where
        Self: Sized,
    {
        EndOnErr::new(self)
    }
}

impl<S: TryStream + ?Sized> ResponseTryStreamExt for S {}

#[pin_project]
pub struct ForwardErr<St, Si, E> {
    #[pin]
    stream: St,
    #[pin]
    sink: Si,
    error: Option<E>,
    sink_fused: bool,
}

impl<St, Si, E> ForwardErr<St, Si, E> {
    pub fn new(stream: St, sink: Si) -> Self {
        Self {
            stream,
            sink,
            error: None,
            sink_fused: false,
        }
    }

    pub fn get_ref(&self) -> &St {
        &self.stream
    }
}

impl<St, Si> Stream for ForwardErr<St, Si, St::Error>
where
    St: TryStream,
    Si: Sink<St::Error>,
{
    type Item = St::Ok;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // If there's an outstanding error, attempt to send it.
        ready!(poll_send(
            ctx,
            this.sink.as_mut(),
            this.error,
            this.sink_fused
        ));

        loop {
            match ready!(this.stream.as_mut().try_poll_next(ctx)) {
                Some(Ok(item)) => return Poll::Ready(Some(item)),
                Some(Err(e)) => {
                    // Got an error; try to send it into the Sink. Since the Sink
                    // may need to be polled multiple times, we need to buffer the
                    // error until it has been sent.
                    *this.error = Some(e);
                    ready!(poll_send(
                        ctx,
                        this.sink.as_mut(),
                        this.error,
                        this.sink_fused
                    ));
                }
                None => {
                    // Close the sink, dropping the returned Result upon completion
                    // since there's nothing we can do with the sink error here.
                    let _ = ready!(this.sink.as_mut().poll_close(ctx));
                    return Poll::Ready(None);
                }
            }
        }
    }
}

impl<St, Si, E> ErrorMetaProvider<E> for ForwardErr<St, Si, E> {
    fn report_errors(self: Pin<&mut Self>, _: &mut ErrorMeta<E>) {
        // TODO: Coming later in this stack
    }
}

/// Attempt to send an (optional) item into the given Sink.
///
/// If the `Sink` isn't ready accept an additional item just yet, this function
/// will return `Poll::Pending`, making it useful when manuall implementing a
/// `Future` or `Stream`.
///
/// Typically, if a `Sink` returns an error at any point, the `Sink` will be
/// permanently unable to accept more items. To avoid fruitlessly attempting
/// to retry in these situations, the function will set the boolean referred to
/// by `fused` to `true` if the `Sink` returns an error, and will subsequently
/// do nothing if called with `fused` set to `true`.
///
/// Note that this is implemented as a plain function instead of a method so
/// that it works well in conjunction with pin projections. Calling methods that
/// require `self: Pin<&mut Self>` can be problematic when working with pin
/// projections, as act of creating the projection consumes `self`, making such
/// method calls impossible.
fn poll_send<T, Si: Sink<T>>(
    ctx: &mut Context<'_>,
    mut sink: Pin<&mut Si>,
    item: &mut Option<T>,
    fused: &mut bool,
) -> Poll<()> {
    if !*fused && item.is_some() {
        match ready!(sink.as_mut().poll_ready(ctx)) {
            Ok(()) => {
                if sink.as_mut().start_send(item.take().unwrap()).is_err() {
                    *fused = true;
                }
            }
            Err(_) => *fused = true,
        }
    }
    Poll::Ready(())
}

#[pin_project]
pub struct EndOnErr<S, E> {
    #[pin]
    stream: S,
    errored: bool,
    error: Option<E>,
}

impl<S, E> EndOnErr<S, E> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            errored: false,
            error: None,
        }
    }

    pub fn get_ref(&self) -> &S {
        &self.stream
    }
}

impl<S, E> Stream for EndOnErr<S, E>
where
    S: TryStream<Error = E>,
{
    type Item = S::Ok;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();

        // Fuse the stream once the error callback has fired.
        if *this.errored {
            return Poll::Ready(None);
        }

        match ready!(this.stream.try_poll_next(ctx)) {
            Some(Ok(item)) => Poll::Ready(Some(item)),
            Some(Err(e)) => {
                this.error.replace(e);
                *this.errored = true;
                Poll::Ready(None)
            }
            None => Poll::Ready(None),
        }
    }
}

impl<S, E> ErrorMetaProvider<E> for EndOnErr<S, E>
where
    S: TryStream<Error = E>,
{
    fn report_errors(self: Pin<&mut Self>, error_meta: &mut ErrorMeta<E>) {
        let this = self.project();
        error_meta.errors.extend(this.error.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::channel::mpsc;

    #[tokio::test]
    async fn test_forward_err() {
        let s = stream::iter(vec![Ok("hello"), Err("foo"), Ok("world"), Err("bar")]);

        let (tx, rx) = mpsc::unbounded();

        let items = s.forward_err(tx).collect::<Vec<_>>().await;
        let errors = rx.collect::<Vec<_>>().await;

        assert_eq!(&items, &["hello", "world"]);
        assert_eq!(&errors, &["foo", "bar"]);
    }

    #[tokio::test]
    async fn test_end_on_err() {
        let s = stream::iter(vec![
            Ok("hello"),
            Ok("world"),
            Err("error"),
            Ok("foo"),
            Err("bar"),
        ])
        .end_on_err();

        futures::pin_mut!(s);

        assert_eq!(s.next().await, Some("hello"));
        assert_eq!(s.next().await, Some("world"));
        assert_eq!(s.next().await, None);

        let mut errors = ErrorMeta::new();
        s.report_errors(&mut errors);
        assert_eq!(&errors.errors, &["error"]);
    }
}
