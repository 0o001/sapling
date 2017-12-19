// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use ascii::AsciiString;

use part_header::PartHeader;

pub use failure::{Error, Result, ResultExt};

#[derive(Debug, Fail)]
pub enum ErrorKind {
    #[fail(display = "bundle2 decode error: {}", _0)] Bundle2Decode(String),
    #[fail(display = "changegroup2 decode error: {}", _0)] Cg2Decode(String),
    #[fail(display = "changegroup2 encode error: {}", _0)] Cg2Encode(String),
    #[fail(display = "wirepack decode error: {}", _0)] WirePackDecode(String),
    #[fail(display = "wirepack encode error: {}", _0)] WirePackEncode(String),
    #[fail(display = "bundle2 encode error: {}", _0)] Bundle2Encode(String),
    #[fail(display = "bundle2 chunk error: {}", _0)] Bundle2Chunk(String),
    #[fail(display = "invalid delta: {}", _0)] InvalidDelta(String),
    #[fail(display = "invalid wire pack entry: {}", _0)] InvalidWirePackEntry(String),
    #[fail(display = "unknown part type: {:?}", _0)] BundleUnknownPart(PartHeader),
    #[fail(display = "unknown params for bundle2 part '{}': {:?}", _0, _1)]
    BundleUnknownPartParams(AsciiString, Vec<String>),
    #[fail(display = "error while generating listkey part")] ListkeyGeneration,
}

impl ErrorKind {
    pub fn is_app_error(&self) -> bool {
        match self {
            &ErrorKind::BundleUnknownPart(_) | &ErrorKind::BundleUnknownPartParams(..) => true,
            _ => false,
        }
    }
}
