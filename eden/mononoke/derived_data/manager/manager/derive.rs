/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::{HashMap, HashSet};
use std::future;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Error, Result};
use async_recursion::async_recursion;
use blobstore::Loadable;
use borrowed::borrowed;
use cloned::cloned;
use context::CoreContext;
use futures::future::{abortable, try_join, AbortHandle, Aborted, FutureExt, TryFutureExt};
use futures::join;
use futures::stream::{self, FuturesUnordered, StreamExt, TryStreamExt};
use futures_stats::{TimedFutureExt, TimedTryFutureExt};
use mononoke_types::ChangesetId;
use slog::debug;
use topo_sort::TopoSortedDagTraversal;

use crate::context::DerivationContext;
use crate::derivable::{BonsaiDerivable, DerivationDependencies};
use crate::error::DerivationError;

use super::{DerivationAssignment, DerivedDataManager};

#[derive(Clone, Copy)]
pub enum BatchDeriveOptions {
    Parallel { gap_size: Option<usize> },
    Serial,
}

pub enum BatchDeriveStats {
    Parallel(Duration),
    Serial(Vec<(ChangesetId, Duration)>),
}

impl BatchDeriveStats {
    fn append(self, other: Self) -> anyhow::Result<Self> {
        use BatchDeriveStats::*;
        Ok(match (self, other) {
            (Parallel(d1), Parallel(d2)) => Parallel(d1 + d2),
            (Serial(mut s1), Serial(mut s2)) => {
                s1.append(&mut s2);
                Serial(s1)
            }
            _ => anyhow::bail!("Incompatible stats"),
        })
    }
}

/// Trait to allow determination of rederivation.
pub trait Rederivation: Send + Sync + 'static {
    /// Determine whether a changeset needs rederivation of
    /// a particular derived data type.
    ///
    /// If this function returns `None`, then it will only be
    /// derived if it isn't already derived.
    fn needs_rederive(&self, derivable_name: &str, csid: ChangesetId) -> Option<bool>;

    /// Marks a changeset as having been derived.  After this
    /// is called, `needs_rederive` should not return `true` for
    /// this changeset.
    fn mark_derived(&self, derivable_name: &str, csid: ChangesetId);
}

impl DerivedDataManager {
    #[async_recursion]
    /// Returns the appropriate manager to derive given changeset, either this
    /// manager, or some secondary manager in the chain.
    async fn get_manager(
        &self,
        ctx: &CoreContext,
        cs_id: ChangesetId,
    ) -> anyhow::Result<&DerivedDataManager> {
        Ok(if let Some(secondary) = &self.inner.secondary {
            if secondary
                .assigner
                .assign(ctx, vec![cs_id])
                .await?
                .secondary
                .is_empty()
            {
                self
            } else {
                secondary.manager.get_manager(ctx, cs_id).await?
            }
        } else {
            self
        })
    }

    pub fn derivation_context(
        &self,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> DerivationContext {
        DerivationContext::new(self.clone(), rederivation, self.repo_blobstore().boxed())
    }

    pub async fn check_derived<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
    ) -> Result<(), DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        if self
            .fetch_derived::<Derivable>(ctx, csid, None)
            .await?
            .is_none()
        {
            return Err(
                anyhow!("expected {} already derived for {}", Derivable::NAME, csid).into(),
            );
        }
        Ok(())
    }

    /// Perform derivation for a single changeset.
    /// Will fail in case data for parents changeset wasn't derived
    pub async fn perform_single_derivation<Derivable>(
        &self,
        ctx: &CoreContext,
        derivation_ctx: &DerivationContext,
        csid: ChangesetId,
    ) -> Result<(ChangesetId, Derivable)>
    where
        Derivable: BonsaiDerivable,
    {
        let mut scuba = ctx.scuba().clone();
        scuba
            .add("changeset_id", csid.to_string())
            .add("derived_data_type", Derivable::NAME);
        scuba
            .clone()
            .log_with_msg("Waiting for derived data to be generated", None);

        debug!(ctx.logger(), "derive {} for {}", Derivable::NAME, csid);
        let lease_key = format!("repo{}.{}.{}", self.repo_id(), Derivable::NAME, csid);

        let ctx = ctx.clone_and_reset();

        let (stats, result) = async {
            let bonsai = csid.load(&ctx, self.repo_blobstore()).map_err(Error::from);
            let guard = async {
                if derivation_ctx.needs_rederive::<Derivable>(csid) {
                    // We are rederiving this changeset, so do not try to take
                    // the lease, as doing so will drop out immediately
                    // because the data is already derived.
                    None
                } else {
                    Some(
                        self.lease()
                            .try_acquire_in_loop(&ctx, &lease_key, || async {
                                Ok(Derivable::fetch(&ctx, derivation_ctx, csid)
                                    .await?
                                    .is_some())
                            })
                            .await,
                    )
                }
            };
            let (bonsai, guard) = join!(bonsai, guard);
            if matches!(guard, Some(Ok(None))) {
                // Something else completed derivation
                let derived = Derivable::fetch(&ctx, derivation_ctx, csid)
                    .await?
                    .ok_or_else(|| {
                        anyhow!("derivation completed elsewhere but data could not be fetched")
                    })?;
                Ok((csid, derived))
            } else {
                // We must perform derivation.  Use the appropriate session
                // class for derivation.
                let ctx = self.set_derivation_session_class(ctx.clone());

                // The derivation process is additonally logged to the derived
                // data scuba table.
                let mut derived_data_scuba = self.derived_data_scuba::<Derivable>(csid);
                self.log_derivation_start::<Derivable>(&ctx, &mut derived_data_scuba, csid);

                let (derive_stats, derived) = async {
                    let bonsai = bonsai?;
                    let parents = derivation_ctx.fetch_parents(&ctx, &bonsai).await?;
                    Derivable::derive_single(&ctx, derivation_ctx, bonsai, parents).await
                }
                .timed()
                .await;

                self.log_derivation_end::<Derivable>(
                    &ctx,
                    &mut derived_data_scuba,
                    csid,
                    &derive_stats,
                    derived.as_ref().err(),
                );

                let derived = derived?;

                // Flush the blobstore.  If it has been set up to cache
                // writes, these must be flushed before we write the mapping.
                derivation_ctx.flush(&ctx).await?;

                // We may now store the mapping, and flush the blobstore to
                // ensure the mapping is persisted.
                let (persist_stats, persisted) = derived
                    .clone()
                    .store_mapping(&ctx, derivation_ctx, csid)
                    .timed()
                    .await;
                derivation_ctx.flush(&ctx).await?;

                self.log_mapping_insertion(
                    &ctx,
                    &mut derived_data_scuba,
                    &derived,
                    &persist_stats,
                    persisted.as_ref().err(),
                );

                persisted?;

                Ok((csid, derived))
            }
        }
        .timed()
        .await;
        scuba.add_future_stats(&stats);
        if result.is_ok() {
            scuba.log_with_msg("Got derived data", None);
        } else {
            scuba.log_with_msg("Failed to get derived data", None);
        };
        result
    }

    /// Find ancestors of the target changeset that are underived.
    async fn find_underived_inner<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        limit: Option<u64>,
        derivation_ctx: &DerivationContext,
    ) -> Result<HashMap<ChangesetId, Vec<ChangesetId>>>
    where
        Derivable: BonsaiDerivable,
    {
        // Ensure we don't visit the same commit multiple times in mergy repos
        let visited: Mutex<HashSet<ChangesetId>> = Default::default();
        borrowed!(visited);
        let underived_commits_parents: HashMap<ChangesetId, Vec<ChangesetId>> =
            bounded_traversal::bounded_traversal_stream(100, Some(csid).into_iter(), {
                move |csid| {
                    async move {
                        if let Some(limit) = limit {
                            let visited = visited.lock().unwrap();
                            if visited.len() as u64 > limit {
                                return Ok::<_, Error>((None, Vec::new()));
                            }
                        }
                        if derivation_ctx
                            .fetch_derived::<Derivable>(ctx, csid)
                            .await?
                            .is_some()
                        {
                            Ok((None, Vec::new()))
                        } else {
                            let parents = self
                                .changesets()
                                .get(ctx.clone(), csid)
                                .await?
                                .ok_or_else(|| anyhow!("changeset not found: {}", csid))?
                                .parents;
                            let mut visited = visited.lock().unwrap();
                            let parents_to_visit = parents
                                .iter()
                                .cloned()
                                .filter(|p| visited.insert(*p))
                                .collect::<Vec<_>>();
                            Ok((Some((csid, parents)), parents_to_visit))
                        }
                    }
                    .boxed()
                }
            })
            .try_filter_map(|underived| async { Ok(underived) })
            .try_collect()
            .await?;

        // Remove parents that have already been derived.
        let underived_commits_parents = underived_commits_parents
            .iter()
            .map(|(csid, parents)| {
                let parents = parents
                    .iter()
                    .filter(|p| underived_commits_parents.contains_key(p))
                    .cloned()
                    .collect::<Vec<_>>();
                (*csid, parents)
            })
            .collect::<HashMap<_, _>>();

        Ok(underived_commits_parents)
    }

    /// Find which ancestors of `csid` are not yet derived, and necessary for
    /// the derivation of `csid` to complete, and derive them.
    async fn derive_underived<Derivable>(
        &self,
        ctx: &CoreContext,
        derivation_ctx: Arc<DerivationContext>,
        target_csid: ChangesetId,
    ) -> Result<DerivationOutcome<Derivable>, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        let (find_underived_stats, mut dag_traversal) = async {
            self.find_underived_inner::<Derivable>(ctx, target_csid, None, derivation_ctx.as_ref())
                .await
                .map(TopoSortedDagTraversal::new)
        }
        .try_timed()
        .await?;

        let buffer_size = self.max_parallel_derivations();
        let mut derivations = FuturesUnordered::new();
        let mut completed_count = 0;
        let mut target_derived = None;
        while !dag_traversal.is_empty() || !derivations.is_empty() {
            let free = buffer_size.saturating_sub(derivations.len());
            derivations.extend(dag_traversal.drain(free).map(|csid| {
                cloned!(ctx, derivation_ctx);
                let manager = self.clone();
                let derivation = async move {
                    manager
                        .perform_single_derivation(&ctx, &derivation_ctx, csid)
                        .await
                };
                tokio::spawn(derivation).map_err(Error::from)
            }));
            if let Some(derivation_result) = derivations.try_next().await? {
                let (derived_csid, derived) = derivation_result?;
                if derived_csid == target_csid {
                    target_derived = Some(derived);
                }
                dag_traversal.visited(derived_csid);
                completed_count += 1;
                derivation_ctx.mark_derived::<Derivable>(derived_csid);
            }
        }

        let derived = match target_derived {
            Some(derived) => derived,
            None => {
                // We didn't find the derived data during derivation, as
                // possibly it was already derived, so just try to fetch it.
                derivation_ctx
                    .fetch_derived(ctx, target_csid)
                    .await?
                    .ok_or_else(|| anyhow!("failed to derive target"))?
            }
        };

        Ok(DerivationOutcome {
            derived,
            count: completed_count,
            find_underived_time: find_underived_stats.completion_time,
        })
    }

    /// Count how many ancestors of `csid` are not yet derived.
    pub async fn count_underived<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        limit: Option<u64>,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<u64, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.get_manager(ctx, csid)
            .await?
            .count_underived_impl::<Derivable>(ctx, csid, limit, rederivation)
            .await
    }

    async fn count_underived_impl<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        limit: Option<u64>,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<u64, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.check_enabled::<Derivable>()?;
        let derivation_ctx = self.derivation_context(rederivation);
        let underived = self
            .find_underived_inner::<Derivable>(ctx, csid, limit, &derivation_ctx)
            .await?;
        Ok(underived.len() as u64)
    }

    /// Find which ancestors of `csid` are not yet derived.
    ///
    /// Searches backwards looking for the most recent ancestors which have
    /// been derived, and returns all of their descendants up to the target
    /// changeset.
    ///
    /// Note that gapped derivation may mean that some of the ancestors
    /// of those changesets may also be underived.  These changesets are not
    /// necessary to derive data for the target changeset, and so will
    /// not be included.
    ///
    /// Returns a map of underived changesets to their underived parents,
    /// suitable for input to toposort.
    pub async fn find_underived<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        limit: Option<u64>,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<HashMap<ChangesetId, Vec<ChangesetId>>>
    where
        Derivable: BonsaiDerivable,
    {
        self.get_manager(ctx, csid)
            .await?
            .find_underived_impl::<Derivable>(ctx, csid, limit, rederivation)
            .await
    }

    async fn find_underived_impl<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        limit: Option<u64>,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<HashMap<ChangesetId, Vec<ChangesetId>>>
    where
        Derivable: BonsaiDerivable,
    {
        self.check_enabled::<Derivable>()?;
        let derivation_ctx = self.derivation_context(rederivation);
        self.find_underived_inner::<Derivable>(ctx, csid, limit, &derivation_ctx)
            .await
    }

    /// Derive or retrieve derived data for a changeset.
    pub async fn derive<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<Derivable, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.get_manager(ctx, csid)
            .await?
            .derive_impl::<Derivable>(ctx, csid, rederivation)
            .await
    }

    async fn derive_impl<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<Derivable, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.check_enabled::<Derivable>()?;
        let derivation_ctx = self.derivation_context(rederivation);

        let pc = ctx.clone().fork_perf_counters();

        let (derivation_task, derivation_abort_handle) =
            abortable(self.derive_underived(ctx, Arc::new(derivation_ctx), csid));
        let (watcher_task, watcher_abort_handle) = abortable(derivation_disabled_watcher(
            self.repo_name().to_string(),
            Derivable::NAME,
            derivation_abort_handle,
        ));
        tokio::spawn(watcher_task);

        let (stats, derivation_result) = derivation_task.timed().await;
        watcher_abort_handle.abort();

        let derivation_result = match derivation_result {
            Ok(result) => result,
            Err(Aborted) => {
                // Derivation was disabled during the derivation process.
                Err(DerivationError::Disabled(
                    Derivable::NAME,
                    self.repo_id(),
                    self.repo_name().to_string(),
                ))
            }
        };

        if self.should_log_slow_derivation(stats.completion_time) {
            self.log_slow_derivation(ctx, csid, &stats, &pc, &derivation_result);
        }

        let derivation_outcome = derivation_result?;
        Ok(derivation_outcome.derived)
    }

    #[async_recursion]
    /// Backfill derived data for a batch of changesets.
    ///
    /// The provided batch of changesets must be in topological
    /// order.
    ///
    /// The difference between "backfill" and "derive", is that for
    /// backfilling, the caller must have arranged for the dependencies
    /// and ancestors of the batch to have already been derived.  If
    /// any dependency or ancestor is not already derived, an error
    /// will be returned.
    pub async fn backfill_batch<Derivable>(
        &self,
        ctx: &CoreContext,
        csids: Vec<ChangesetId>,
        batch_options: BatchDeriveOptions,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<BatchDeriveStats, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        let (csids, secondary_derivation) = if let Some(secondary_data) = &self.inner.secondary {
            let DerivationAssignment { primary, secondary } =
                secondary_data.assigner.assign(ctx, csids).await?;
            (primary, {
                cloned!(rederivation);
                async move {
                    secondary_data
                        .manager
                        .backfill_batch::<Derivable>(ctx, secondary, batch_options, rederivation)
                        .await
                }
                .left_future()
            })
        } else {
            (
                csids,
                future::ready(Ok(match batch_options {
                    BatchDeriveOptions::Serial => BatchDeriveStats::Serial(vec![]),
                    BatchDeriveOptions::Parallel { .. } => {
                        BatchDeriveStats::Parallel(Duration::ZERO)
                    }
                }))
                .right_future(),
            )
        };
        self.check_enabled::<Derivable>()?;
        let mut derivation_ctx = self.derivation_context(rederivation);

        // Enable write batching, so that writes are stored in memory
        // before being flushed.
        derivation_ctx.enable_write_batching();
        borrowed!(derivation_ctx);

        let mut scuba = ctx.scuba().clone();
        scuba
            .add("stack_size", csids.len())
            .add("derived_data", Derivable::NAME);
        if let (Some(first), Some(last)) = (csids.first(), csids.last()) {
            scuba
                .add("first_csid", first.to_string())
                .add("last_csid", last.to_string());
        }

        // Load all of the bonsais for this batch.
        let bonsais = stream::iter(csids.into_iter().map(|csid| async move {
            let bonsai = csid.load(ctx, derivation_ctx.blobstore()).await?;
            Ok::<_, Error>(bonsai)
        }))
        .buffered(100)
        .try_collect::<Vec<_>>()
        .await?;

        // Dependency checks: check topological order and determine heads
        // and highest ancestors of the batch.
        let mut seen = HashSet::new();
        let mut heads = HashSet::new();
        let mut ancestors = HashSet::new();
        for bonsai in bonsais.iter() {
            let csid = bonsai.get_changeset_id();
            if ancestors.contains(&csid) {
                return Err(anyhow!("batch not in topological order at {}", csid).into());
            }
            for parent in bonsai.parents() {
                if !seen.contains(&parent) {
                    ancestors.insert(parent);
                }
                heads.remove(&parent);
            }
            seen.insert(csid);
            heads.insert(csid);
        }

        // Dependency checks: all ancestors should have this derived
        // data type derived
        let ancestor_checks = async move {
            stream::iter(ancestors)
                .map(|csid| derivation_ctx.fetch_dependency::<Derivable>(ctx, csid))
                .buffered(100)
                .try_for_each(|_| async { Ok(()) })
                .await
                .with_context(|| {
                    format!(
                        "a batch ancestor does not have '{}' derived",
                        Derivable::NAME
                    )
                })
        };

        // Dependency checks: all heads should have their dependent
        // data types derived.
        let dependency_checks = async move {
            stream::iter(heads)
                .map(|csid| async move {
                    Derivable::Dependencies::check_dependencies(
                        ctx,
                        derivation_ctx,
                        csid,
                        &mut HashSet::new(),
                    )
                    .await
                })
                .buffered(100)
                .try_for_each(|_| async { Ok(()) })
                .await
                .context("a batch dependency has not been derived")
        };

        try_join(ancestor_checks, dependency_checks)
            .await
            .context("backfill batch pre-conditions not satisfied")?;

        let ctx = self.set_derivation_session_class(ctx.clone());
        borrowed!(ctx);

        if let (Some(first), Some(last)) = (bonsais.first(), bonsais.last()) {
            debug!(
                ctx.logger(),
                "backfill {} batch from {} to {}",
                Derivable::NAME,
                first.get_changeset_id(),
                last.get_changeset_id()
            );
        }

        let (batch_stats, derived) = match batch_options {
            BatchDeriveOptions::Parallel { gap_size } => {
                let (stats, derived) =
                    Derivable::derive_batch(ctx, derivation_ctx, bonsais, gap_size)
                        .try_timed()
                        .await?;
                (BatchDeriveStats::Parallel(stats.completion_time), derived)
            }
            BatchDeriveOptions::Serial => {
                let mut per_commit_stats = Vec::new();
                let mut per_commit_derived = HashMap::new();
                for bonsai in bonsais {
                    let csid = bonsai.get_changeset_id();
                    let parents = derivation_ctx
                        .fetch_unknown_parents(ctx, Some(&per_commit_derived), &bonsai)
                        .await?;
                    let (stats, derived) =
                        Derivable::derive_single(ctx, derivation_ctx, bonsai, parents)
                            .try_timed()
                            .await?;
                    per_commit_stats.push((csid, stats.completion_time));
                    per_commit_derived.insert(csid, derived);
                }
                (
                    BatchDeriveStats::Serial(per_commit_stats),
                    per_commit_derived,
                )
            }
        };

        // Flush the blobstore.  If it has been set up to cache writes, these
        // must be flushed before we write the mapping.
        let (stats, _) = derivation_ctx.flush(ctx).try_timed().await?;
        scuba
            .add_future_stats(&stats)
            .log_with_msg("Flushed derived blobs", None);

        // Write all mapping values, and flush the blobstore to ensure they
        // are persisted.
        let (stats, _) = async {
            let csids = stream::iter(derived.into_iter())
                .map(|(csid, derived)| async move {
                    derived.store_mapping(ctx, derivation_ctx, csid).await?;
                    Ok::<_, Error>(csid)
                })
                .buffer_unordered(100)
                .try_collect::<Vec<_>>()
                .await?;

            derivation_ctx.flush(ctx).await?;
            for csid in csids {
                derivation_ctx.mark_derived::<Derivable>(csid);
            }
            Ok::<_, Error>(())
        }
        .try_timed()
        .await?;
        scuba
            .add_future_stats(&stats)
            .log_with_msg("Flushed mapping", None);

        Ok(batch_stats.append(secondary_derivation.await?)?)
    }

    /// Fetch derived data for a changeset if it has previously been derived.
    pub async fn fetch_derived<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<Option<Derivable>, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.get_manager(ctx, csid)
            .await?
            .fetch_derived_impl::<Derivable>(ctx, csid, rederivation)
            .await
    }

    async fn fetch_derived_impl<Derivable>(
        &self,
        ctx: &CoreContext,
        csid: ChangesetId,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<Option<Derivable>, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        self.check_enabled::<Derivable>()?;
        let derivation_ctx = self.derivation_context(rederivation);
        let derived = derivation_ctx.fetch_derived::<Derivable>(ctx, csid).await?;
        Ok(derived)
    }

    #[async_recursion]
    /// Fetch derived data for a batch of changesets if they have previously
    /// been derived.
    ///
    /// Returns a hashmap from changeset id to the derived data.  Changesets
    /// for which the data has not previously been derived are omitted.
    pub async fn fetch_derived_batch<Derivable>(
        &self,
        ctx: &CoreContext,
        csids: Vec<ChangesetId>,
        rederivation: Option<Arc<dyn Rederivation>>,
    ) -> Result<HashMap<ChangesetId, Derivable>, DerivationError>
    where
        Derivable: BonsaiDerivable,
    {
        let (csids, secondary_derivation) = if let Some(secondary_data) = &self.inner.secondary {
            let DerivationAssignment { primary, secondary } =
                secondary_data.assigner.assign(ctx, csids).await?;
            (primary, {
                cloned!(rederivation);
                async move {
                    secondary_data
                        .manager
                        .fetch_derived_batch::<Derivable>(ctx, secondary, rederivation)
                        .await
                }
                .left_future()
            })
        } else {
            (csids, future::ready(Ok(HashMap::new())).right_future())
        };
        self.check_enabled::<Derivable>()?;
        let derivation_ctx = self.derivation_context(rederivation);
        let mut derived = derivation_ctx
            .fetch_derived_batch::<Derivable>(ctx, csids)
            .await?;
        derived.extend(secondary_derivation.await?);
        Ok(derived)
    }
}

pub(super) struct DerivationOutcome<Derivable> {
    /// The derived data.
    pub(super) derived: Derivable,

    /// Number of changesets that were derived.
    pub(super) count: u64,

    /// Time take to find the underived changesets.
    pub(super) find_underived_time: Duration,
}

fn emergency_disabled(repo_name: &str, derivable_name: &str) -> bool {
    let disabled_for_repo = tunables::tunables()
        .get_by_repo_all_derived_data_disabled(repo_name)
        .unwrap_or(false);

    if disabled_for_repo {
        return true;
    }

    let disabled_for_type = tunables::tunables()
        .get_by_repo_derived_data_types_disabled(repo_name)
        .unwrap_or(vec![]);

    if disabled_for_type
        .iter()
        .any(|ty| ty.as_str() == derivable_name)
    {
        return true;
    }

    // Not disabled
    false
}

async fn derivation_disabled_watcher(
    repo_name: String,
    derivable_name: &'static str,
    abort_handle: AbortHandle,
) {
    let mut delay_secs = tunables::tunables().get_derived_data_disabled_watcher_delay_secs();
    if delay_secs <= 0 {
        delay_secs = 10;
    }
    let delay_duration = Duration::from_secs(delay_secs as u64);
    loop {
        if emergency_disabled(&repo_name, derivable_name) {
            abort_handle.abort();
            break;
        }
        tokio::time::sleep(delay_duration).await;
    }
}
