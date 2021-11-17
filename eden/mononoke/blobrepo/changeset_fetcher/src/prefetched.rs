/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{bail, format_err, Error, Result};
use changesets::{ChangesetEntry, Changesets};
use context::CoreContext;
use futures::stream::{Stream, TryStreamExt};
use mononoke_types::{ChangesetId, Generation, RepositoryId};
use std::collections::HashMap;
use std::sync::Arc;

use crate::ChangesetFetcher;

/// A [`ChangesetFetcher`] that uses prefetched data as an optimization to
/// speed up fetching by storing many entries in memory, rather than querying
/// changeset by changeset and relying on caching
pub struct PrefetchedChangesetsFetcher {
    changesets: Arc<dyn Changesets>,
    prefetched: HashMap<ChangesetId, ChangesetEntry>,
}

impl PrefetchedChangesetsFetcher {
    /// Construct with a fetcher to get from the backing store, and a prefetched set
    /// This can come directly from bulkops::PublicChangesetBulkFetch::fetch
    /// or from a deserialised file via `futures::stream::iter`
    pub async fn new(
        repo_id: RepositoryId,
        changesets: Arc<dyn Changesets>,
        prefetched: impl Stream<Item = Result<ChangesetEntry, Error>>,
    ) -> Result<Self> {
        if changesets.repo_id() != repo_id {
            bail!("Changesets object and supplied repo ID do not match");
        }
        let prefetched = prefetched
            .and_then(|entry| async move {
                if entry.repo_id != repo_id {
                    bail!("Prefetched changesets and supplied repo ID do not match");
                }
                Ok((entry.cs_id, entry))
            })
            .try_collect()
            .await?;
        Ok(Self {
            changesets,
            prefetched,
        })
    }

    async fn get_cs_entry(&self, ctx: CoreContext, cs_id: ChangesetId) -> Result<ChangesetEntry> {
        let prefetched_entry = self.prefetched.get(&cs_id);
        if let Some(prefetched_entry) = prefetched_entry {
            Ok(prefetched_entry.clone())
        } else {
            let maybe_cs = self.changesets.get(ctx, cs_id).await?;
            maybe_cs.ok_or_else(|| format_err!("{} not found", cs_id))
        }
    }
}

#[async_trait::async_trait]
impl ChangesetFetcher for PrefetchedChangesetsFetcher {
    async fn get_generation_number(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> Result<Generation, Error> {
        let cs = self.get_cs_entry(ctx, cs_id).await?;
        Ok(Generation::new(cs.gen))
    }

    async fn get_parents(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> Result<Vec<ChangesetId>, Error> {
        let cs = self.get_cs_entry(ctx, cs_id).await?;
        Ok(cs.parents)
    }
}
