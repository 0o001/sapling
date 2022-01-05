/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! nodemap - A store for node-to-node mappings, with bidirectional indexes.

pub mod nodemap;
pub mod nodeset;

pub use indexedlog::Repair;

pub use crate::nodemap::NodeMap;
pub use crate::nodeset::NodeSet;
