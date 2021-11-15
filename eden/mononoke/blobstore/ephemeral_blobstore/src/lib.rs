/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Ephemeral Blobstore
//!
//! Unlike other blobstores, this blobstore organises blobs into ephemeral
//! "bubbles".  Blobs placed within a bubble are deleted when the bubble
//! expires.  A bubble's lifespan can be extended at any time before it
//! expires.

mod bubble;
mod builder;
mod changesets;
mod error;
mod handle;
mod store;
mod view;

pub use crate::bubble::{Bubble, BubbleId, StorageLocation};
pub use crate::builder::RepoEphemeralBlobstoreBuilder;
pub use crate::changesets::EphemeralChangesets;
pub use crate::error::EphemeralBlobstoreError;
pub use crate::handle::EphemeralHandle;
pub use crate::store::{ArcRepoEphemeralBlobstore, RepoEphemeralBlobstore};
pub use crate::view::EphemeralRepoView;
