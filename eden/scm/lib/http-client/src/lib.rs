/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! An async-compatible HTTP client built on top of libcurl.

#![deny(warnings)]

mod client;
mod driver;
mod errors;
mod handler;
mod header;
mod pool;
mod progress;
mod receiver;
mod request;
mod response;
mod stats;
mod stream;

pub use client::{HttpClient, ResponseStream, StatsFuture};
pub use curl::easy::HttpVersion;
pub use errors::{Abort, HttpClientError};
pub use header::Header;
pub use progress::Progress;
pub use receiver::Receiver;
pub use request::{Method, MinTransferSpeed, Request, StreamRequest};
pub use response::{AsyncBody, AsyncResponse, Response};
pub use stats::Stats;
pub use stream::{BufferedStream, CborStream};
