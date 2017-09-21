// Copyright (c) 2017-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use mercurial_types::NodeHash;

error_chain! {
    errors {
        NoSuchNode(hash: NodeHash) {
            description("node not found in repo")
            display("no such node: {}", hash)
        }
        GenerationFetchFailed {
            description("could not fetch node generation")
            display("could not fetch node generation")
        }
    }
}
