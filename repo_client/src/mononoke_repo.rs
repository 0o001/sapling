// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use crate::errors::*;
use crate::read_write::RepoReadWriteFetcher;
use blobrepo::BlobRepo;
use blobstore::Blobstore;
use futures_ext::BoxFuture;
use hooks::HookManager;
use metaconfig_types::{
    BookmarkOrRegex, BookmarkParams, LfsParams, PushrebaseParams, RepoReadOnly,
};
use mononoke_types::RepositoryId;
use prefixblob::PrefixBlobstore;
use std::fmt::{self, Debug};
use std::sync::Arc;
use streaming_clone::SqlStreamingChunksFetcher;

#[derive(Clone)]
pub struct SqlStreamingCloneConfig {
    pub blobstore: PrefixBlobstore<Arc<Blobstore>>,
    pub fetcher: SqlStreamingChunksFetcher,
    pub repoid: RepositoryId,
}

#[derive(Clone)]
pub struct MononokeRepo {
    blobrepo: BlobRepo,
    pushrebase_params: PushrebaseParams,
    fastforward_only_bookmarks: Vec<BookmarkOrRegex>,
    hook_manager: Arc<HookManager>,
    streaming_clone: Option<SqlStreamingCloneConfig>,
    lfs_params: LfsParams,
    reponame: String,
    readonly_fetcher: RepoReadWriteFetcher,
}

impl MononokeRepo {
    #[inline]
    pub fn new(
        blobrepo: BlobRepo,
        pushrebase_params: &PushrebaseParams,
        bookmark_params: Vec<BookmarkParams>,
        hook_manager: Arc<HookManager>,
        streaming_clone: Option<SqlStreamingCloneConfig>,
        lfs_params: LfsParams,
        reponame: String,
        readonly_fetcher: RepoReadWriteFetcher,
    ) -> Self {
        let fastforward_only_bookmarks = bookmark_params
            .into_iter()
            .filter_map(|param| {
                if param.only_fast_forward {
                    Some(param.bookmark)
                } else {
                    None
                }
            })
            .collect();
        MononokeRepo {
            blobrepo,
            pushrebase_params: pushrebase_params.clone(),
            fastforward_only_bookmarks,
            hook_manager,
            streaming_clone,
            lfs_params,
            reponame,
            readonly_fetcher,
        }
    }

    #[inline]
    pub fn blobrepo(&self) -> &BlobRepo {
        &self.blobrepo
    }

    pub fn pushrebase_params(&self) -> &PushrebaseParams {
        &self.pushrebase_params
    }

    pub fn fastforward_only_bookmarks(&self) -> &Vec<BookmarkOrRegex> {
        &self.fastforward_only_bookmarks
    }

    pub fn hook_manager(&self) -> Arc<HookManager> {
        self.hook_manager.clone()
    }

    pub fn streaming_clone(&self) -> &Option<SqlStreamingCloneConfig> {
        &self.streaming_clone
    }

    pub fn lfs_params(&self) -> &LfsParams {
        &self.lfs_params
    }

    pub fn reponame(&self) -> &String {
        &self.reponame
    }

    pub fn readonly(&self) -> BoxFuture<RepoReadOnly, Error> {
        self.readonly_fetcher.readonly()
    }
}

pub fn streaming_clone(
    blobrepo: BlobRepo,
    db_address: &str,
    myrouter_port: u16,
    repoid: RepositoryId,
) -> Result<SqlStreamingCloneConfig> {
    let fetcher = SqlStreamingChunksFetcher::with_myrouter(db_address, myrouter_port);
    let streaming_clone = SqlStreamingCloneConfig {
        fetcher,
        blobstore: blobrepo.get_blobstore(),
        repoid,
    };

    Ok(streaming_clone)
}

impl Debug for MononokeRepo {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "MononokeRepo({:#?})", self.blobrepo.get_repoid())
    }
}
