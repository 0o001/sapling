// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use errors::ErrorKind;

use errors::*;

bitflags! {
    // names are from hg revlog.py
    pub struct RevFlags: u16 {
        const REVIDX_DEFAULT_FLAGS = 0;
        const REVIDX_EXTSTORED = 1 << 13;  // data is stored externally
        // Unused, not supported yet
        const REVIDX_ELLIPSIS = 1 << 14;  // revision hash does not match data (narrowhg)
        // Unused, not supported yet
        const REVIDX_ISCENSORED = 1 << 15; // revision has censor metadata, must be verified
    }
}

pub fn parse_rev_flags(flags: Option<u16>) -> Result<RevFlags> {
    // None -> Default
    // Some(valid) -> Ok(valid_flags)
    // Some(invalid) -> Err()
    match flags {
        Some(value) => match RevFlags::from_bits(value) {
            Some(value) => Ok(value),
            None => Err(ErrorKind::UnknownRevFlags.into()),
        },
        None => Ok(RevFlags::REVIDX_DEFAULT_FLAGS),
    }
}
