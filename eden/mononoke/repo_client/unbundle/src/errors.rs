/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use mercurial_types::HgChangesetId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ErrorKind {
    #[error("Error while uploading data for changesets, hashes: {0:?}")]
    WhileUploadingData(Vec<HgChangesetId>),
    #[error("Repo is marked as read-only: {0}")]
    RepoReadOnly(String),
}
