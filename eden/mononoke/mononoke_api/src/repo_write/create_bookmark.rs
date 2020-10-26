/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::sync::Arc;

use bookmarks::{BookmarkName, BookmarkUpdateReason};
use bytes::Bytes;
use metaconfig_types::BookmarkAttrs;
use mononoke_types::ChangesetId;
use reachabilityindex::LeastCommonAncestorsHint;

use crate::errors::MononokeError;
use crate::repo_write::{PermissionsModel, RepoWriteContext};

impl RepoWriteContext {
    /// Create a bookmark.
    pub async fn create_bookmark(
        &self,
        bookmark: impl AsRef<str>,
        target: ChangesetId,
        pushvars: Option<&HashMap<String, Bytes>>,
    ) -> Result<(), MononokeError> {
        let bookmark = bookmark.as_ref();
        self.check_method_permitted("create_bookmark")?;

        let bookmark = BookmarkName::new(bookmark)?;
        let bookmark_attrs = BookmarkAttrs::new(self.config().bookmarks.clone());

        let lca_hint: Arc<dyn LeastCommonAncestorsHint> = self.skiplist_index().clone();

        // Create the bookmark.
        let mut op = bookmarks_movement::CreateBookmarkOp::new(
            &bookmark,
            target,
            BookmarkUpdateReason::ApiRequest,
        )
        .with_pushvars(pushvars);

        if let PermissionsModel::ServiceIdentity(service_identity) = &self.permissions_model {
            op = op.for_service(service_identity, &self.config().source_control_service);
        }

        op.run(
            self.ctx(),
            self.blob_repo(),
            &lca_hint,
            &self.config().infinitepush,
            &self.config().pushrebase,
            &bookmark_attrs,
            self.hook_manager().as_ref(),
            self.readonly_fetcher(),
        )
        .await?;

        Ok(())
    }
}
