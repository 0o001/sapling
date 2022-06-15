/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use blobstore::Loadable;
use bookmarks::BookmarkName;
use bytes::Bytes;
use cloned::cloned;
use futures::compat::Stream01CompatExt;
use futures::future::{self, TryFutureExt};
use futures::stream::TryStreamExt;
use hooks::CrossRepoPushSource;
use metaconfig_types::BookmarkAttrs;
use mononoke_types::ChangesetId;
use reachabilityindex::LeastCommonAncestorsHint;
use revset::RangeNodeStream;

use crate::errors::MononokeError;
use crate::permissions::WritePermissionsModel;
use crate::repo_write::RepoWriteContext;

pub use bookmarks_movement::PushrebaseOutcome;

impl RepoWriteContext {
    /// Land a stack of commits to a bookmark via pushrebase.
    pub async fn land_stack(
        &self,
        bookmark: impl AsRef<str>,
        head: ChangesetId,
        base: ChangesetId,
        pushvars: Option<&HashMap<String, Bytes>>,
        push_source: CrossRepoPushSource,
    ) -> Result<PushrebaseOutcome, MononokeError> {
        let bookmark = bookmark.as_ref();
        self.check_method_permitted("land_stack")?;

        let bookmark = BookmarkName::new(bookmark)?;
        let bookmark_attrs =
            BookmarkAttrs::new(self.ctx().fb, self.config().bookmarks.clone()).await?;

        let lca_hint: Arc<dyn LeastCommonAncestorsHint> = self.skiplist_index().clone();

        // Check that base is an ancestor of the head commit, and fail with an
        // appropriate error message if that's not the case.
        if !lca_hint
            .is_ancestor(
                self.ctx(),
                &self.blob_repo().get_changeset_fetcher(),
                base,
                head,
            )
            .await?
        {
            return Err(MononokeError::InvalidRequest(format!(
                "Not a stack: base commit {} is not an ancestor of head commit {}",
                base, head,
            )));
        }

        // Find the commits we are interested in, and load their bonsai
        // changesets.   These are the commits that are ancestors of the head
        // commit and descendants of the base commit.
        let ctx = self.ctx();
        let blobstore = self.blob_repo().blobstore();
        let changesets: HashSet<_> = RangeNodeStream::new(
            ctx.clone(),
            self.blob_repo().get_changeset_fetcher(),
            base,
            head,
        )
        .compat()
        .map_err(MononokeError::from)
        .try_filter(|cs_id| future::ready(*cs_id != base))
        .map_ok(|cs_id| {
            cloned!(ctx);
            async move {
                cs_id
                    .load(&ctx, blobstore)
                    .map_err(MononokeError::from)
                    .await
            }
        })
        .try_buffer_unordered(100)
        .try_collect()
        .await?;

        // Pushrebase these commits onto the bookmark.
        let mut op = bookmarks_movement::PushrebaseOntoBookmarkOp::new(&bookmark, changesets)
            .with_pushvars(pushvars)
            .with_push_source(push_source);

        if let WritePermissionsModel::ServiceIdentity(service_identity) = &self.permissions_model {
            op = op.for_service(service_identity, &self.config().source_control_service);
        }

        let outcome = op
            .run(
                self.ctx(),
                self.inner_repo(),
                &lca_hint,
                &self.config().infinitepush,
                &self.config().pushrebase,
                &bookmark_attrs,
                self.hook_manager().as_ref(),
                self.readonly_fetcher(),
            )
            .await?;

        Ok(outcome)
    }
}
