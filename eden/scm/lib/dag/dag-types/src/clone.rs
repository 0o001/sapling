/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::segment::PreparedFlatSegments;

#[derive(Clone, Debug, PartialEq, Eq)]
#[derive(Serialize, Deserialize)]
pub struct CloneData<Name> {
    pub flat_segments: PreparedFlatSegments,
    pub idmap: HashMap<Id, Name>,
}

#[cfg(any(test, feature = "for-tests"))]
use quickcheck::Arbitrary;

#[cfg(any(test, feature = "for-tests"))]
impl<Name> Arbitrary for CloneData<Name>
where
    Name: Arbitrary,
{
    fn arbitrary<G: quickcheck::Gen>(g: &mut G) -> Self {
        let flat_segments = PreparedFlatSegments {
            segments: Vec::arbitrary(g),
        };
        CloneData {
            flat_segments,
            idmap: HashMap::arbitrary(g),
        }
    }
}
