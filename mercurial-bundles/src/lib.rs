// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]
#![feature(ascii_ctype)]

extern crate ascii;
#[macro_use]
#[cfg(test)]
extern crate assert_matches;
extern crate byteorder;
extern crate bytes;
#[macro_use]
extern crate failure_ext as failure;
#[macro_use]
extern crate futures;
extern crate futures_trace;
#[cfg(test)]
#[macro_use]
extern crate itertools;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate maplit;
#[cfg(not(test))]
extern crate quickcheck;
#[cfg(test)]
#[macro_use]
extern crate quickcheck;
#[macro_use]
extern crate slog;
#[cfg(test)]
extern crate slog_term;
extern crate tokio;
extern crate tokio_codec;
extern crate tokio_io;
extern crate tokio_proto;
extern crate url;

extern crate async_compression;
extern crate bytes_ext;
extern crate futures_ext;
extern crate mercurial;
extern crate mercurial_types;
#[cfg(test)]
extern crate mercurial_types_mocks;
#[cfg(test)]
extern crate partial_io;

pub mod bundle2;
pub mod bundle2_encode;
pub mod changegroup;
pub mod infinitepush;
mod capabilities;
mod chunk;
mod delta;
pub mod parts;
pub mod part_encode;
mod part_header;
mod part_inner;
mod part_outer;
mod pushrebase;
mod quickcheck_types;
mod stream_start;
mod types;
pub mod wirepack;
#[cfg(test)]
mod test;

mod errors;
pub use errors::*;
mod utils;

use bytes::Bytes;
use failure::err_msg;
use futures::{Future, Stream};
use futures::sync::{mpsc, oneshot};
use futures_ext::SinkToAsyncWrite;

use std::fmt;

use futures_ext::{BoxFuture, BoxStream};

pub use bundle2_encode::Bundle2EncodeBuilder;
pub use part_header::{PartHeader, PartHeaderType};
pub use types::StreamHeader;

pub enum Bundle2Item {
    Start(StreamHeader),
    Changegroup(PartHeader, BoxStream<changegroup::Part, Error>),
    B2xCommonHeads(PartHeader, BoxStream<mercurial_types::HgChangesetId, Error>),
    B2xInfinitepush(PartHeader, BoxStream<changegroup::Part, Error>),
    B2xTreegroup2(PartHeader, BoxStream<wirepack::Part, Error>),
    // B2xInfinitepushBookmarks returns Bytes because this part is not going to be used.
    B2xInfinitepushBookmarks(PartHeader, BoxStream<bytes::Bytes, Error>),
    Replycaps(PartHeader, BoxFuture<capabilities::Capabilities, Error>),
    Pushkey(PartHeader, BoxFuture<(), Error>),
}

impl Bundle2Item {
    pub fn is_start(&self) -> bool {
        match self {
            &Bundle2Item::Start(_) => true,
            _ => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn unwrap_start(self) -> StreamHeader {
        match self {
            Bundle2Item::Start(stream_header) => stream_header,
            other => panic!("expected item to be Start, was {:?}", other),
        }
    }
}

impl fmt::Debug for Bundle2Item {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Bundle2Item::*;
        match self {
            &Start(ref header) => write!(f, "Bundle2Item::Start({:?})", header),
            &Changegroup(ref header, _) => write!(f, "Bundle2Item::Changegroup({:?}, ...)", header),
            &B2xCommonHeads(ref header, _) => {
                write!(f, "Bundle2Item::B2xCommonHeads({:?}, ...)", header)
            }
            &B2xInfinitepush(ref header, _) => {
                write!(f, "Bundle2Item::B2xInfinitepush({:?}, ...)", header)
            }
            &B2xInfinitepushBookmarks(ref header, _) => write!(
                f,
                "Bundle2Item::B2xInfinitepushBookmarks({:?}, ...)",
                header
            ),
            &B2xTreegroup2(ref header, _) => {
                write!(f, "Bundle2Item::B2xTreegroup2({:?}, ...)", header)
            }
            &Replycaps(ref header, _) => write!(f, "Bundle2Item::Replycaps({:?}, ...)", header),
            &Pushkey(ref header, _) => write!(f, "Bundle2Item::Pushkey({:?}, ...)", header),
        }
    }
}

/// Given bundle parts, returns a stream of Bytes that represent an encoded bundle with these parts
pub fn create_bundle_stream<C: Into<Option<async_compression::CompressorType>>>(
    parts: Vec<part_encode::PartEncodeBuilder>,
    ct: C,
) -> impl Stream<Item = bytes::Bytes, Error = Error> {
    let (sender, receiver) = mpsc::channel::<Bytes>(1);
    // Sends either and empty Bytes if bundle generation was successful or an error.
    // Empty Bytes are used just to make chaining of streams below easier.
    let (result_sender, result_receiver) = oneshot::channel::<Result<Bytes>>();
    // Bundle2EncodeBuilder accepts writer which implements AsyncWrite. To workaround that we
    // use SinkToAsyncWrite. It implements AsyncWrite trait and sends everything that was written
    // into the Sender
    let mut bundle = Bundle2EncodeBuilder::new(SinkToAsyncWrite::new(sender));
    bundle.set_compressor_type(ct);
    for part in parts {
        bundle.add_part(part);
    }

    tokio::spawn(bundle.build().then(move |val| {
        // Ignore send errors, because they can only happen if receiver was deallocated already
        match val {
            Ok(_) => {
                // Bundle was successfully generated, so there is nothing add.
                // So just add empty bytes.
                let _ = result_sender.send(Ok(Bytes::new()));
            }
            Err(err) => {
                let _ = result_sender.send(Err(err));
            }
        };
        Ok(())
    }));

    receiver
        .map(|bytes| Ok(bytes))
        .chain(result_receiver.into_stream().map_err(|_err| ()))
        .then(|entry| match entry {
            Ok(res) => res,
            Err(()) => Err(err_msg(
                "error while receiving gettreepack response from the channel",
            )),
        })
}
