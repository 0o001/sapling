/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use async_trait::async_trait;
use changesets::{ChangesetEntry, ChangesetInsert, Changesets, SortOrder};
use cloned::cloned;
use context::CoreContext;
use futures::channel::mpsc::Sender;
use futures::sink::SinkExt;
use futures::stream::BoxStream;
use mononoke_types::{
    ChangesetId, ChangesetIdPrefix, ChangesetIdsResolvedFromPrefix, RepositoryId,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct MicrowaveChangesets {
    repo_id: RepositoryId,
    recorder: Sender<ChangesetEntry>,
    inner: Arc<dyn Changesets>,
}

impl MicrowaveChangesets {
    pub fn new(
        repo_id: RepositoryId,
        recorder: Sender<ChangesetEntry>,
        inner: Arc<dyn Changesets>,
    ) -> Self {
        Self {
            repo_id,
            recorder,
            inner,
        }
    }
}

#[async_trait]
impl Changesets for MicrowaveChangesets {
    async fn add(&self, _ctx: CoreContext, cs: ChangesetInsert) -> Result<bool, Error> {
        // See rationale in filenodes.rs for why we error out on unexpected calls under
        // MicrowaveFilenodes.
        unimplemented!("MicrowaveChangesets: unexpected add in repo {}", cs.repo_id)
    }

    async fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> Result<Option<ChangesetEntry>, Error> {
        cloned!(self.inner, mut self.recorder);

        // NOTE: See MicrowaveFilenodes for context on this.
        assert_eq!(repo_id, self.repo_id);
        let entry = inner.get(ctx, repo_id, cs_id).await?;

        if let Some(ref entry) = entry {
            assert_eq!(entry.repo_id, repo_id); // Same as above
            recorder.send(entry.clone()).await?;
        }

        Ok(entry)
    }

    async fn get_many(
        &self,
        _ctx: CoreContext,
        repo_id: RepositoryId,
        _cs_ids: Vec<ChangesetId>,
    ) -> Result<Vec<ChangesetEntry>, Error> {
        // Same as above
        unimplemented!(
            "MicrowaveChangesets: unexpected get_many in repo {}",
            repo_id
        )
    }

    async fn get_many_by_prefix(
        &self,
        _ctx: CoreContext,
        repo_id: RepositoryId,
        _cs_prefix: ChangesetIdPrefix,
        _limit: usize,
    ) -> Result<ChangesetIdsResolvedFromPrefix, Error> {
        // Same as above
        unimplemented!(
            "MicrowaveChangesets: unexpected get_many_by_prefix in repo {}",
            repo_id
        )
    }

    fn prime_cache(&self, ctx: &CoreContext, changesets: &[ChangesetEntry]) {
        self.inner.prime_cache(ctx, changesets)
    }

    async fn enumeration_bounds(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        read_from_master: bool,
    ) -> Result<Option<(u64, u64)>, Error> {
        self.inner
            .enumeration_bounds(ctx, repo_id, read_from_master)
            .await
    }

    fn list_enumeration_range(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        min_id: u64,
        max_id: u64,
        sort_and_limit: Option<(SortOrder, u64)>,
        read_from_master: bool,
    ) -> BoxStream<'_, Result<(ChangesetId, u64), Error>> {
        self.inner.list_enumeration_range(
            ctx,
            repo_id,
            min_id,
            max_id,
            sort_and_limit,
            read_from_master,
        )
    }
}
