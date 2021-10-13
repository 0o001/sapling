/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::convert::TryInto;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use context::CoreContext;
use futures::future::{abortable, AbortHandle};
use mononoke_types::ChangesetId;
use rand::Rng;
use slog::info;
use tokio::sync::Notify;
use tunables::tunables;

use crate::manager::SegmentedChangelogManager;
use crate::{segmented_changelog_delegate, CloneData, Location, SegmentedChangelog};
use reloader::{Loader, Reloader};

struct SegmentedChangelogLoader {
    manager: SegmentedChangelogManager,
    ctx: CoreContext,
}

type LoadedSegmentedChangelog = Arc<dyn SegmentedChangelog + Send + Sync>;

#[async_trait]
impl Loader<LoadedSegmentedChangelog> for SegmentedChangelogLoader {
    async fn load(&mut self) -> Result<Option<LoadedSegmentedChangelog>> {
        Ok(Some(self.manager.load(&self.ctx).await?))
    }
}

pub struct PeriodicReloadSegmentedChangelog(Reloader<LoadedSegmentedChangelog>, AbortHandle);

impl PeriodicReloadSegmentedChangelog {
    pub async fn start<L: Loader<LoadedSegmentedChangelog> + Send + Sync + 'static>(
        ctx: &CoreContext,
        period: Duration,
        loader: L,
        name: String,
    ) -> Result<Self> {
        let force_reload_notify = Arc::new(Notify::new());

        let ctx_clone = ctx.clone();
        let force_reload_notify_clone = force_reload_notify.clone();

        // This is a future to trigger force reload of segmented changelog
        let fut = async move {
            let mut force_reload_val =
                tunables().get_by_repo_segmented_changelog_force_reload(&name);
            loop {
                let mut jitter = tunables().get_segmented_changelog_force_reload_jitter_secs();
                if jitter <= 0 {
                    jitter = 30;
                }
                let jitter = rand::thread_rng().gen_range(
                    Duration::from_secs(0)..Duration::from_secs(jitter.try_into().unwrap()),
                );
                tokio::time::sleep(jitter).await;

                let new_force_reload_val =
                    tunables().get_by_repo_segmented_changelog_force_reload(&name);
                if force_reload_val != new_force_reload_val {
                    info!(ctx_clone.logger(), "force reloading segmented changelog");
                    force_reload_notify_clone.notify_waiters();
                    force_reload_val = new_force_reload_val;
                }
            }
        };

        let (fut, abort_handle) = abortable(fut);
        tokio::spawn(fut);

        Ok(Self(
            Reloader::reload_periodically_with_skew_and_force_reload(
                ctx.clone(),
                period,
                loader,
                force_reload_notify,
            )
            .await?,
            abort_handle,
        ))
    }

    pub async fn start_from_manager(
        ctx: &CoreContext,
        period: Duration,
        manager: SegmentedChangelogManager,
        name: String,
    ) -> Result<Self> {
        Self::start(
            ctx,
            period,
            SegmentedChangelogLoader {
                manager,
                ctx: ctx.clone(),
            },
            name,
        )
        .await
    }

    #[cfg(test)]
    pub async fn wait_for_update(&self) {
        self.0.wait_for_update().await
    }
}

segmented_changelog_delegate!(PeriodicReloadSegmentedChangelog, |
    &self,
    ctx: &CoreContext,
| { self.0.load() });

impl Drop for PeriodicReloadSegmentedChangelog {
    fn drop(&mut self) {
        self.1.abort()
    }
}
