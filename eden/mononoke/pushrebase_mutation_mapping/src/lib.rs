/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

mod save_mapping_pushrebase_hook;
mod sql_queries;
#[cfg(test)]
mod test;

use mononoke_types::{ChangesetId, RepositoryId};
use pushrebase_hook::PushrebaseHook;

pub use sql_queries::{
    add_pushrebase_mapping, get_prepushrebase_ids, SqlPushrebaseMutationMapping,
    SqlPushrebaseMutationMappingConnection,
};

pub struct PushrebaseMutationMappingEntry {
    repo_id: RepositoryId,
    predecessor_bcs_id: ChangesetId,
    successor_bcs_id: ChangesetId,
}

impl PushrebaseMutationMappingEntry {
    fn new(
        repo_id: RepositoryId,
        predecessor_bcs_id: ChangesetId,
        successor_bcs_id: ChangesetId,
    ) -> Self {
        Self {
            repo_id,
            predecessor_bcs_id,
            successor_bcs_id,
        }
    }
}

#[facet::facet]
pub trait PushrebaseMutationMapping: Send + Sync {
    fn get_hook(&self) -> Option<Box<dyn PushrebaseHook>>;
}
