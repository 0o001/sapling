// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

pub use failure::{Error, Result};

#[derive(Debug, Fail)]
pub enum ErrorKind {
    #[fail(display = "Unimplmented oepration '{}'", _0)] Unimplemented(String),
    #[fail(display = "command parse failed for '{}'", _0)] CommandParse(String),
    #[fail(display = "malformed batch with command '{}'", _0)] BatchInvalid(String),
    #[fail(display = "unknown escape character in batch command '{}'", _0)] BatchEscape(u8),
    #[fail(display = "Repo error")] RepoError,
    #[fail(display = "cannot serve revlog repos")] CantServeRevlogRepo,
}
