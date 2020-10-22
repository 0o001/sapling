/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use chrono::{DateTime, Utc};
use mononoke_types::{ChangesetId, Generation, RepositoryId};
use permission_checker::MononokeIdentitySet;
use scribe_ext::Scribe;
use serde_derive::Serialize;

#[derive(Serialize)]
pub struct CommitInfo<'a> {
    repo_id: RepositoryId,
    #[serde(skip_serializing_if = "Option::is_none")]
    bookmark: Option<&'a str>,
    generation: Generation,
    changeset_id: ChangesetId,
    parents: Vec<ChangesetId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_unix_name: Option<&'a str>,
    #[serde(skip_serializing_if = "MononokeIdentitySet::is_empty")]
    user_identities: &'a MononokeIdentitySet,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_hostname: Option<&'a str>,
    #[serde(with = "::chrono::serde::ts_seconds")]
    received_timestamp: DateTime<Utc>,
}

impl<'a> CommitInfo<'a> {
    pub fn new(
        repo_id: RepositoryId,
        bookmark: Option<&'a str>,
        generation: Generation,
        changeset_id: ChangesetId,
        parents: Vec<ChangesetId>,
        user_unix_name: Option<&'a str>,
        user_identities: &'a MononokeIdentitySet,
        source_hostname: Option<&'a str>,
        received_timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            repo_id,
            bookmark,
            generation,
            changeset_id,
            parents,
            user_unix_name,
            user_identities,
            source_hostname,
            received_timestamp,
        }
    }
}

pub struct LogToScribe {
    client: Option<Scribe>,
    category: String,
}

impl LogToScribe {
    pub fn new(client: Scribe, category: String) -> Self {
        Self {
            client: Some(client),
            category,
        }
    }

    pub fn new_with_discard() -> Self {
        Self {
            client: None,
            category: String::new(),
        }
    }

    pub fn queue_commit(&self, commit: &CommitInfo<'_>) -> Result<(), Error> {
        match &self.client {
            Some(ref client) => {
                let commit = serde_json::to_string(commit)?;
                client.offer(&self.category, &commit)
            }
            None => Ok(()),
        }
    }
}
