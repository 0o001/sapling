// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Type definitions for inner streams.
#![deny(warnings)]

use std::collections::{HashMap, HashSet};
use std::io::BufRead;
use std::str;

use slog;

use bytes::{Bytes, BytesMut};
use futures::{future, Future, Stream};
use futures_ext::{BoxFuture, FutureExt, StreamWrapper};
use tokio_io::AsyncRead;
use tokio_io::codec::Decoder;

use Bundle2Item;
use capabilities;
use changegroup;
use errors::*;
use futures_ext::{StreamExt, StreamLayeredExt};
use infinitepush;
use part_header::{PartHeader, PartHeaderType};
use part_outer::{OuterFrame, OuterStream};
use pushrebase;
use wirepack;

// --- Part parameters

lazy_static! {
    static ref KNOWN_PARAMS: HashMap<PartHeaderType, HashSet<&'static str>> = {
        let mut m: HashMap<PartHeaderType, HashSet<&'static str>> = HashMap::new();
        m.insert(PartHeaderType::Changegroup, hashset!{"version", "nbchanges", "treemanifest"});
        // TODO(stash): currently ignore all the parameters. Later we'll
        // support 'bookmark' parameter, and maybe 'create' and 'force' (although 'force' will
        // probably) be renamed T26385545. 'bookprevnode' and 'pushbackbookmarks' will be
        // removed T26384190.
        m.insert(PartHeaderType::B2xInfinitepush, hashset!{
            "pushbackbookmarks", "cgversion", "bookmark", "bookprevnode", "create", "force"});
        m.insert(PartHeaderType::B2xInfinitepushBookmarks, hashset!{});
        m.insert(PartHeaderType::B2xCommonHeads, hashset!{});
        m.insert(PartHeaderType::B2xRebase, hashset!{"onto", "newhead", "cgversion", "obsmarkerversions"});
        m.insert(PartHeaderType::B2xRebasePack, hashset!{"version", "cache", "category"});
        m.insert(PartHeaderType::B2xTreegroup2, hashset!{"version", "cache", "category"});
        m.insert(PartHeaderType::Replycaps, hashset!{});
        m.insert(PartHeaderType::Pushkey, hashset!{ "namespace", "key", "old", "new" });
        m.insert(PartHeaderType::Pushvars, hashset!{});
        m
    };
}

pub fn validate_header(header: PartHeader) -> Result<Option<PartHeader>> {
    match KNOWN_PARAMS.get(header.part_type()) {
        Some(ref known_params) => {
            // Make sure all the mandatory params are recognized.
            let unknown_params: Vec<_> = header
                .mparams()
                .keys()
                .filter(|param| !known_params.contains(param.as_str()))
                .map(|param| param.clone())
                .collect();
            if !unknown_params.is_empty() {
                bail_err!(ErrorKind::BundleUnknownPartParams(
                    *header.part_type(),
                    unknown_params,
                ));
            }
            Ok(Some(header))
        }
        None => {
            if header.mandatory() {
                bail_err!(ErrorKind::BundleUnknownPart(header));
            }
            Ok(None)
        }
    }
}

pub fn get_cg_unpacker(
    header: PartHeader,
    logger: slog::Logger,
) -> changegroup::unpacker::CgUnpacker {
    // TODO(anastasiyaz): T34812941 return Result here, no default packer (version should be specified)
    header
    .aparams()
    .get(changegroup::CG_PART_VERSION_HEADER_NAME)
    .ok_or(ErrorKind::CgDecode("No changegroup version in Part Header in aparams".into()))
    .and_then(|version_bytes| str::from_utf8(version_bytes).map_err(|e| ErrorKind::CgDecode(format!("{:?}", e))))
    .and_then(|version_str| version_str.parse::<changegroup::unpacker::CgVersion>().map_err(|e| ErrorKind::CgDecode(format!("{:?}", e))))
    .map(|version| changegroup::unpacker::CgUnpacker::new(logger.clone(), version))
    // ChangeGroup2 by default
    .unwrap_or_else(|e| {
        warn!(logger, "{:?}", e);
        let default_version = changegroup::unpacker::CgVersion::Cg2Version;
        changegroup::unpacker::CgUnpacker::new(logger, default_version)
    })
}

/// Convert an OuterStream into an InnerStream using the part header.
pub fn inner_stream<R: AsyncRead + BufRead + 'static + Send>(
    header: PartHeader,
    stream: OuterStream<R>,
    logger: &slog::Logger,
) -> (Bundle2Item, BoxFuture<OuterStream<R>, Error>) {
    let wrapped_stream = stream
        .take_while_wrapper(|frame| future::ok(frame.is_payload()))
        .map(OuterFrame::get_payload as fn(OuterFrame) -> Bytes);
    let (wrapped_stream, remainder) = wrapped_stream.return_remainder();

    let bundle2item = match header.part_type() {
        &PartHeaderType::Changegroup => {
            let cg2_stream = wrapped_stream.decode(get_cg_unpacker(
                header.clone(),
                logger.new(o!("stream" => "cg2")),
            ));
            Bundle2Item::Changegroup(header, Box::new(cg2_stream))
        }
        &PartHeaderType::B2xCommonHeads => {
            let heads_stream = wrapped_stream.decode(pushrebase::CommonHeadsUnpacker::new());
            Bundle2Item::B2xCommonHeads(header, Box::new(heads_stream))
        }
        &PartHeaderType::B2xInfinitepush => {
            let cg2_stream = wrapped_stream.decode(get_cg_unpacker(
                header.clone(),
                logger.new(o!("stream" => "cg2")),
            ));
            Bundle2Item::B2xInfinitepush(header, Box::new(cg2_stream))
        }
        &PartHeaderType::B2xInfinitepushBookmarks => {
            let bookmarks_stream =
                wrapped_stream.decode(infinitepush::InfinitepushBookmarksUnpacker::new());
            Bundle2Item::B2xInfinitepushBookmarks(header, Box::new(bookmarks_stream))
        }
        &PartHeaderType::B2xTreegroup2 => {
            let wirepack_stream = wrapped_stream.decode(wirepack::unpacker::new(
                logger.new(o!("stream" => "wirepack")),
                // Mercurial only knows how to send trees at the moment.
                // TODO: add support for file wirepacks once that's a thing
                wirepack::Kind::Tree,
            ));
            Bundle2Item::B2xTreegroup2(header, Box::new(wirepack_stream))
        }
        &PartHeaderType::Replycaps => {
            let caps = wrapped_stream
                .decode(capabilities::CapabilitiesUnpacker)
                .collect()
                .and_then(|caps| {
                    ensure_msg!(caps.len() == 1, "Unexpected Replycaps payload: {:?}", caps);
                    Ok(caps.into_iter().next().unwrap())
                });
            Bundle2Item::Replycaps(header, Box::new(caps))
        }
        &PartHeaderType::B2xRebasePack => {
            let wirepack_stream = wrapped_stream.decode(wirepack::unpacker::new(
                logger.new(o!("stream" => "wirepack")),
                // Mercurial only knows how to send trees at the moment.
                // TODO: add support for file wirepacks once that's a thing
                wirepack::Kind::Tree,
            ));
            Bundle2Item::B2xRebasePack(header, Box::new(wirepack_stream))
        }
        &PartHeaderType::B2xRebase => {
            let cg2_stream = wrapped_stream.decode(get_cg_unpacker(
                header.clone(),
                logger.new(o!("stream" => "cg2")),
            ));
            Bundle2Item::B2xRebase(header, Box::new(cg2_stream))
        }
        &PartHeaderType::Pushkey => {
            // Pushkey part has an empty part payload, but we still need to "parse" it
            // Otherwise polling remainder stream may fail.
            let empty = wrapped_stream.decode(EmptyUnpacker).for_each(|_| Ok(()));
            Bundle2Item::Pushkey(header, Box::new(empty))
        }
        &PartHeaderType::Pushvars => {
            // Pushvars part has an empty part payload, but we still need to "parse" it
            // Otherwise polling remainder stream may fail.
            let empty = wrapped_stream.decode(EmptyUnpacker).for_each(|_| Ok(()));
            Bundle2Item::Pushvars(header, Box::new(empty))
        }
        _ => panic!("TODO: make this an error"),
    };

    (
        bundle2item,
        remainder
            .map(|s| s.into_inner().into_inner())
            .from_err()
            .boxify(),
    )
}

// Decoder for an empty part (for example, pushkey)
pub struct EmptyUnpacker;

impl Decoder for EmptyUnpacker {
    type Item = ();
    type Error = Error;

    fn decode(&mut self, _buf: &mut BytesMut) -> Result<Option<Self::Item>> {
        Ok(None)
    }
}
