/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! An async-compatible HTTP client built on top of libcurl.

#![allow(dead_code)]

mod client;
mod driver;
mod errors;
mod event_listeners;
mod handler;
mod header;
mod pool;
mod progress;
mod receiver;
mod request;
mod response;
mod stats;
mod stream;

pub use client::{HttpClient, ResponseFuture, StatsFuture};
pub use curl::easy::HttpVersion;
pub use errors::{Abort, HttpClientError, TlsError};
pub use header::Header;
pub use progress::Progress;
pub use receiver::Receiver;
pub use request::{
    Encoding, Method, MinTransferSpeed, Request, RequestContext, RequestInfo, StreamRequest,
};
pub use response::{AsyncBody, AsyncResponse, Response};
pub use stats::Stats;
pub use stream::{BufferedStream, CborStream};
