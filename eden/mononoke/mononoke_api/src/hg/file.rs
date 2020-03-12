/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use blobrepo::file_history::get_file_history;
use blobstore::{Loadable, LoadableError};
use bytes::Bytes;
use futures::{
    compat::{Future01CompatExt, Stream01CompatExt},
    TryStream, TryStreamExt,
};
use mercurial_types::{envelope::HgFileEnvelope, HgFileHistoryEntry, HgFileNodeId, HgParents};
use mononoke_types::MPath;
use remotefilelog::create_getpack_v1_blob;

use crate::errors::MononokeError;

use super::HgRepoContext;

/// An abstraction around a Mercurial filenode.
///
/// In Mercurial's data model, a filenode is addressed by its content along with
/// its history -- a filenode ID is a hash of the file content and its parents'
/// filenode hashes. Notably, filenodes are not addressed by the path of the file
/// within the repo; as such, perhaps counterintuitively, an HgFileContext is not
/// aware of the path to the file to which it refers.
#[derive(Clone)]
pub struct HgFileContext {
    repo: HgRepoContext,
    envelope: HgFileEnvelope,
}

impl HgFileContext {
    /// Create a new `HgFileContext`. The file must exist in the repository.
    ///
    /// To construct an `HgFileContext` for a file that may not exist, use
    /// `new_check_exists`.
    pub async fn new(
        repo: HgRepoContext,
        filenode_id: HgFileNodeId,
    ) -> Result<Self, MononokeError> {
        // Fetch and store Mononoke's internal representation of the metadata of this
        // file. The actual file contents are not fetched here.
        let ctx = repo.ctx().clone();
        let blobstore = repo.blob_repo().blobstore();
        let envelope = filenode_id.load(ctx, blobstore).compat().await?;
        Ok(Self { repo, envelope })
    }

    pub async fn new_check_exists(
        repo: HgRepoContext,
        filenode_id: HgFileNodeId,
    ) -> Result<Option<Self>, MononokeError> {
        let ctx = repo.ctx().clone();
        let blobstore = repo.blob_repo().blobstore();
        match filenode_id.load(ctx, blobstore).compat().await {
            Ok(envelope) => Ok(Some(Self { repo, envelope })),
            Err(LoadableError::Missing(_)) => Ok(None),
            Err(e) => Err(e)?,
        }
    }

    /// Get the filenode hash (HgFileNodeId) for this file version.
    ///
    /// This should be same as the HgFileNodeId specified when this context was created,
    /// but the value returned here comes from the data loaded from Mononoke.
    pub fn node_id(&self) -> HgFileNodeId {
        self.envelope.node_id()
    }

    /// Get the parents of this file version in a strongly typed way.
    ///
    /// Useful for implementing anything that needs to traverse the history
    /// of file nodes, or otherwise needs to use make further queries using
    /// the returned `HgFileNodeId`s.
    pub fn parents(&self) -> (Option<HgFileNodeId>, Option<HgFileNodeId>) {
        self.envelope.parents()
    }

    /// Get the parents of this file version in a format that can be easily
    /// sent to the Mercurial client as part of a serialized response.
    pub fn hg_parents(&self) -> HgParents {
        self.envelope.hg_parents()
    }

    /// Get the content for this file in the format expected by Mercurial's data storage layer.
    /// In particular, this returns the full content of the file, in some cases prefixed with
    /// a small header. Callers should not assume that the data returned by this function
    /// only contains file content.
    pub async fn content(&self) -> Result<Bytes, MononokeError> {
        let ctx = self.repo.ctx().clone();
        let blob_repo = self.repo.blob_repo().clone();
        let filenode_id = self.node_id();

        // TODO(kulshrax): Update this to use getpack_v2, which supports LFS.
        let (_size, content_fut) = create_getpack_v1_blob(ctx, blob_repo, filenode_id, false)
            .compat()
            .await?;

        // TODO(kulshrax): Right now this buffers the entire file content in memory. It would
        // probably be better for this method to return a stream of the file content instead.
        let (_filenode, content) = content_fut.compat().await?;

        Ok(content)
    }

    /// Get the history of this file (at a particular path in the repo) as a stream of Mercurial
    /// file history entries.
    ///
    /// Note that since this context could theoretically represent a filenode that existed at
    /// multiple paths within the repo (for example, two files with identical content that were
    /// added at different locations), the caller is required to specify the exact path of the
    /// file to query.
    pub fn history(
        &self,
        path: MPath,
        max_depth: Option<u32>,
    ) -> impl TryStream<Ok = HgFileHistoryEntry, Error = MononokeError> {
        let ctx = self.repo.ctx().clone();
        let blob_repo = self.repo.blob_repo().clone();
        let filenode_id = self.node_id();
        get_file_history(ctx, blob_repo, filenode_id, path, max_depth)
            .compat()
            .map_err(MononokeError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{str::FromStr, sync::Arc};

    use context::CoreContext;
    use fbinit::FacebookInit;
    use fixtures::many_files_dirs;
    use mercurial_types::{HgChangesetId, NULL_HASH};

    use crate::repo::{Repo, RepoContext};

    #[fbinit::compat_test]
    async fn test_hg_file_context(fb: FacebookInit) -> Result<(), MononokeError> {
        let ctx = CoreContext::test_mock(fb);
        let repo = Arc::new(Repo::new_test(ctx.clone(), many_files_dirs::getrepo(fb).await).await?);

        // The `many_files_dirs` test repo contains the following files (at tip):
        //   $ hg manifest --debug
        //   b8e02f6433738021a065f94175c7cd23db5f05be 644   1
        //   5d9299349fc01ddd25d0070d149b124d8f10411e 644   2
        //   e2ac7cbe1f85e0d8b416005e905aa2189434ce6c 644   dir1
        //   0eb86721b74ed44cf176ee48b5e95f0192dc2824 644   dir2/file_1_in_dir2

        let repo_ctx = RepoContext::new(ctx, repo)?;
        let hg = repo_ctx.hg();

        // Test HgFileContext::new.
        let file_id = HgFileNodeId::from_str("b8e02f6433738021a065f94175c7cd23db5f05be").unwrap();
        let hg_file = HgFileContext::new(hg.clone(), file_id).await?;

        assert_eq!(file_id, hg_file.node_id());

        let content = hg_file.content().await?;
        assert_eq!(content, &b"1\n"[..]);

        // Test HgFileContext::new_check_exists.
        let hg_file = HgFileContext::new_check_exists(hg.clone(), file_id).await?;
        assert!(hg_file.is_some());

        let null_id = HgFileNodeId::new(NULL_HASH);
        let null_file = HgFileContext::new(hg.clone(), null_id).await;
        assert!(null_file.is_err());

        let null_file = HgFileContext::new_check_exists(hg.clone(), null_id).await?;
        assert!(null_file.is_none());

        Ok(())
    }

    #[fbinit::compat_test]
    async fn test_hg_file_history(fb: FacebookInit) -> Result<(), MononokeError> {
        let ctx = CoreContext::test_mock(fb);
        let repo = Arc::new(Repo::new_test(ctx.clone(), many_files_dirs::getrepo(fb).await).await?);

        // The `many_files_dirs` test repo contains the following files (at tip):
        //   $ hg manifest --debug
        //   b8e02f6433738021a065f94175c7cd23db5f05be 644   1
        //   5d9299349fc01ddd25d0070d149b124d8f10411e 644   2
        //   e2ac7cbe1f85e0d8b416005e905aa2189434ce6c 644   dir1
        //   0eb86721b74ed44cf176ee48b5e95f0192dc2824 644   dir2/file_1_in_dir2

        let repo_ctx = RepoContext::new(ctx, repo)?;
        let hg = repo_ctx.hg();

        // Test HgFileContext::new.
        let file_id = HgFileNodeId::from_str("b8e02f6433738021a065f94175c7cd23db5f05be").unwrap();
        let hg_file = HgFileContext::new(hg.clone(), file_id).await?;

        let path = MPath::new("1")?;
        let history = hg_file.history(path, None).try_collect::<Vec<_>>().await?;

        let expected = vec![HgFileHistoryEntry::new(
            file_id,
            HgParents::None,
            HgChangesetId::new(NULL_HASH),
            None,
        )];
        assert_eq!(history, expected);

        Ok(())
    }
}
