// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

pub use failure::{Error, Result};
use mercurial_types::{HgFileNodeId, RepoPath};

#[derive(Debug, Eq, Fail, PartialEq)]
pub enum ErrorKind {
    #[fail(display = "Invalid copy: {:?} copied from {:?}", _0, _1)]
    InvalidCopy(RepoPath, RepoPath),
    #[fail(
        display = "Internal error: failure while fetching file node {} {}",
        _0, _1
    )]
    FailFetchFilenode(HgFileNodeId, RepoPath),
    #[fail(
        display = "Internal error: failure while fetching copy information {} {}",
        _0, _1
    )]
    FailFetchCopydata(HgFileNodeId, RepoPath),
    #[fail(
        display = "Internal error: copy information is not found for {} {}",
        _0, _1
    )]
    CopydataNotFound(HgFileNodeId, RepoPath),
    #[fail(
        display = "Internal error: failure while fetching file nodes for {}",
        _0
    )]
    FailRangeFetch(RepoPath),
    #[fail(
        display = "Internal error: failure while fetching copy source path for {}",
        _0
    )]
    FromPathNotFound(RepoPath),
}
