// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

//! Extensions for the bytes crate.

#![deny(warnings)]

extern crate bytes;

mod sized;

pub use sized::SizeCounter;
