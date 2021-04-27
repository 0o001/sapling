/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use bookmarks::BookmarkTransactionError;
use bytes::Bytes;
use context::CoreContext;
use metaconfig_types::RepoReadOnly;
use mononoke_types::{BonsaiChangesetMut, ChangesetId};
use pushrebase_hook::{
    PushrebaseCommitHook, PushrebaseHook, PushrebaseTransactionHook, RebasedChangesets,
};
use repo_read_write_status::RepoReadWriteFetcher;
use sql::Transaction;

use crate::restrictions::BookmarkKind;
use crate::BookmarkMovementError;

fn should_check_repo_lock(kind: BookmarkKind, pushvars: Option<&HashMap<String, Bytes>>) -> bool {
    match kind {
        BookmarkKind::Scratch => false,
        BookmarkKind::Public => {
            if let Some(pushvars) = pushvars {
                if let Some(value) = pushvars.get("BYPASS_READONLY") {
                    if value.to_ascii_lowercase() == b"true" {
                        return false;
                    }
                }
            }
            true
        }
    }
}

pub(crate) async fn check_repo_lock(
    repo_read_write_fetcher: &RepoReadWriteFetcher,
    kind: BookmarkKind,
    pushvars: Option<&HashMap<String, Bytes>>,
) -> Result<(), BookmarkMovementError> {
    if should_check_repo_lock(kind, pushvars) {
        let state = repo_read_write_fetcher
            .readonly()
            .await
            .context("Failed to fetch repo lock state")?;
        if let RepoReadOnly::ReadOnly(reason) = state {
            return Err(BookmarkMovementError::RepoLocked(reason));
        }
    }

    Ok(())
}

pub(crate) struct RepoLockPushrebaseHook {
    repo_read_write_fetcher: Arc<RepoReadWriteFetcher>,
}

impl RepoLockPushrebaseHook {
    pub(crate) fn new(
        repo_read_write_fetcher: &RepoReadWriteFetcher,
        kind: BookmarkKind,
        pushvars: Option<&HashMap<String, Bytes>>,
    ) -> Option<Box<dyn PushrebaseHook>> {
        if should_check_repo_lock(kind, pushvars) {
            let hook = Box::new(RepoLockPushrebaseHook {
                repo_read_write_fetcher: Arc::new(repo_read_write_fetcher.clone()),
            });
            Some(hook as Box<dyn PushrebaseHook>)
        } else {
            None
        }
    }
}

#[async_trait]
impl PushrebaseHook for RepoLockPushrebaseHook {
    async fn prepushrebase(&self) -> Result<Box<dyn PushrebaseCommitHook>> {
        let hook = Box::new(RepoLockCommitTransactionHook {
            repo_read_write_fetcher: self.repo_read_write_fetcher.clone(),
        });
        Ok(hook as Box<dyn PushrebaseCommitHook>)
    }
}

struct RepoLockCommitTransactionHook {
    repo_read_write_fetcher: Arc<RepoReadWriteFetcher>,
}

#[async_trait]
impl PushrebaseCommitHook for RepoLockCommitTransactionHook {
    fn post_rebase_changeset(
        &mut self,
        _bcs_old: ChangesetId,
        _bcs_new: &mut BonsaiChangesetMut,
    ) -> Result<()> {
        Ok(())
    }

    async fn into_transaction_hook(
        self: Box<Self>,
        _ctx: &CoreContext,
        _rebased: &RebasedChangesets,
    ) -> Result<Box<dyn PushrebaseTransactionHook>> {
        Ok(self as Box<dyn PushrebaseTransactionHook>)
    }
}

#[async_trait]
impl PushrebaseTransactionHook for RepoLockCommitTransactionHook {
    async fn populate_transaction(
        &self,
        _ctx: &CoreContext,
        txn: Transaction,
    ) -> Result<Transaction, BookmarkTransactionError> {
        let state = self
            .repo_read_write_fetcher
            .readonly()
            .await
            .context("Failed to fetch repo lock state")?;
        if let RepoReadOnly::ReadOnly(reason) = state {
            return Err(BookmarkTransactionError::Other(anyhow!(
                "Repo is locked: {}",
                reason
            )));
        }

        Ok(txn)
    }
}
