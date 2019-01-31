// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]
pub mod errors;
pub use crate::errors::ErrorKind;

mod index;
pub use crate::index::{LeastCommonAncestorsHint, NodeFrontier, ReachabilityIndex};
