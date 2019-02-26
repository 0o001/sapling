// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

use std::collections::HashMap;

use context::CoreContext;
use failure::Error;
use futures::{future::join_all, Future, IntoFuture};
use futures_ext::{BoxFuture, FutureExt};
use slog::Logger;

use metaconfig_parser::RepoConfigs;

use crate::errors::ErrorKind;

mod lfs;
mod model;
mod query;
mod repo;
mod response;

pub use self::lfs::BatchRequest;
pub use self::query::{MononokeQuery, MononokeRepoQuery, Revision};
pub use self::repo::MononokeRepo;
pub use self::response::MononokeRepoResponse;

pub struct Mononoke {
    repos: HashMap<String, MononokeRepo>,
}

impl Mononoke {
    pub fn new(
        logger: Logger,
        config: RepoConfigs,
        myrouter_port: Option<u16>,
        with_skiplist: bool,
    ) -> impl Future<Item = Self, Error = Error> {
        join_all(
            config
                .repos
                .into_iter()
                .filter(move |&(_, ref config)| config.enabled)
                .map({
                    move |(name, config)| {
                        MononokeRepo::new(logger.clone(), config, myrouter_port, with_skiplist)
                            .map(|repo| (name, repo))
                    }
                }),
        )
        .map(move |repos| Self {
            repos: repos.into_iter().collect(),
        })
    }

    pub fn send_query(
        &self,
        ctx: CoreContext,
        MononokeQuery { repo, kind, .. }: MononokeQuery,
    ) -> BoxFuture<MononokeRepoResponse, ErrorKind> {
        match self.repos.get(&repo) {
            Some(repo) => repo.send_query(ctx, kind),
            None => match kind {
                MononokeRepoQuery::LfsBatch { .. } => {
                    // LFS batch request require error in the different format:
                    // json: {"message": "Error message here"}
                    Err(ErrorKind::LFSNotFound(repo)).into_future().boxify()
                }
                _ => Err(ErrorKind::NotFound(repo, None)).into_future().boxify(),
            },
        }
    }
}
