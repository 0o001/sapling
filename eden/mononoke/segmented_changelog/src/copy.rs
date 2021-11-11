/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;

use anyhow::{format_err, Context, Result};
use blobstore::Blobstore;
use context::CoreContext;
use mononoke_types::{ChangesetId, RepositoryId};
use sql_ext::replication::ReplicaLagMonitor;

use crate::iddag::IdDagSaveStore;
use crate::idmap::{IdMap, SqlIdMap, SqlIdMapVersionStore};
use crate::types::{IdMapVersion, SegmentedChangelogVersion};
use crate::version_store::SegmentedChangelogVersionStore;
use crate::{InProcessIdDag, SegmentedChangelogSqlConnections};

pub async fn copy_segmented_changelog(
    ctx: &CoreContext,
    repo_id: RepositoryId,
    connections: SegmentedChangelogSqlConnections,
    blobstore: Arc<dyn Blobstore>,
    replica_lag_monitor: Arc<dyn ReplicaLagMonitor>,
    heads: Vec<ChangesetId>,
) -> Result<()> {
    let idmap_version_store = SqlIdMapVersionStore::new(connections.0.clone(), repo_id);
    let iddag_save_store = IdDagSaveStore::new(repo_id, blobstore);
    let sc_version_store = SegmentedChangelogVersionStore::new(connections.0.clone(), repo_id);

    let new_idmap_version = {
        let v = idmap_version_store
            .get(&ctx)
            .await
            .context("error fetching idmap version from store")?
            .context("no current IdMap version")?;
        IdMapVersion(v.0 + 1)
    };
    let sc_version = sc_version_store
        .get(&ctx)
        .await
        .with_context(|| {
            format!(
                "repo {}: error loading segmented changelog version",
                repo_id
            )
        })?
        .ok_or_else(|| {
            format_err!(
                "repo {}: segmented changelog metadata not found, maybe repo is not seeded",
                repo_id
            )
        })?;

    let old_iddag = iddag_save_store
        .load(&ctx, sc_version.iddag_version)
        .await
        .with_context(|| format!("repo {}: failed to load iddag", repo_id))?;

    let idmap = SqlIdMap::new(
        connections.0,
        replica_lag_monitor,
        repo_id,
        sc_version.idmap_version,
    );

    let dag_limit = idmap
        .find_many_dag_ids(ctx, heads.clone())
        .await?
        .into_values()
        .max()
        .with_context(|| format!("repo {}: no valid heads in {:?}", repo_id, heads))?;

    let _new_idmap = idmap.copy(dag_limit, new_idmap_version).await?;

    // Build an IdDag - we can use the old IdDag's shape to speed things up,
    // as we know that the new IdDag is a subset of the old one.
    let mut new_iddag = InProcessIdDag::new_in_process();
    let get_parents = |id| old_iddag.parent_ids(id);
    new_iddag.build_segments(dag_limit, &get_parents)?;

    let iddag_version = iddag_save_store
        .save(&ctx, &new_iddag)
        .await
        .with_context(|| format!("repo {}: error saving iddag", repo_id))?;

    let sc_version = SegmentedChangelogVersion::new(iddag_version, new_idmap_version);
    sc_version_store
        .set(&ctx, sc_version)
        .await
        .with_context(|| {
            format!(
                "repo {}: error updating segmented changelog version store",
                repo_id
            )
        })?;

    Ok(())
}
