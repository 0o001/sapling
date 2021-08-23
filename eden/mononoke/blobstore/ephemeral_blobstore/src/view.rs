/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use changesets::Changesets;
use repo_blobstore::RepoBlobstore;

#[facet::container]
pub struct EphemeralRepoView {
    #[facet]
    pub(crate) repo_blobstore: RepoBlobstore,

    #[facet]
    pub(crate) changesets: dyn Changesets,
}
