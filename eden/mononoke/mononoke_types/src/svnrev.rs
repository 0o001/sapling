/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::BonsaiChangeset;
use abomonation_derive::Abomonation;
use anyhow::{bail, Error, Result};
use sql::mysql;
use std::str;

// Changeset svnrev. Present only in some repos which were imported from SVN.
#[derive(Abomonation, Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[derive(mysql::OptTryFromRowField)]
pub struct Svnrev(u64);

impl Svnrev {
    #[inline]
    pub const fn new(rev: u64) -> Self {
        Self(rev)
    }

    #[inline]
    pub fn id(&self) -> u64 {
        self.0
    }

    // ex. svn:uuid/path@1234
    pub fn parse_svnrev(svnrev: &str) -> Result<u64> {
        let at_pos = svnrev
            .rfind('@')
            .ok_or_else(|| Error::msg("Wrong convert_revision value"))?;
        let result = svnrev[1 + at_pos..].parse::<u64>()?;
        Ok(result)
    }

    pub fn from_bcs(bcs: &BonsaiChangeset) -> Result<Self> {
        match bcs.extra().find(|(key, _)| key == &"convert_revision") {
            Some((_, svnrev)) => {
                let svnrev = Svnrev::parse_svnrev(str::from_utf8(&svnrev.to_vec())?)?;
                Ok(Self::new(svnrev))
            }
            None => bail!("Bonsai cs {:?} without svnrev", bcs),
        }
    }
}
