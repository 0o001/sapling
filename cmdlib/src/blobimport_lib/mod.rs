// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]

mod bookmark;
mod changeset;

use std::path::PathBuf;
use std::sync::Arc;

use failure::{err_msg, Error};
use futures::{future, Future, Stream};
use futures_ext::{BoxFuture, FutureExt, StreamExt};
use slog::Logger;

use blobrepo::BlobRepo;
use context::CoreContext;
use mercurial::RevlogRepo;
use mercurial_types::HgNodeHash;

use self::changeset::UploadChangesets;

pub struct Blobimport {
    pub ctx: CoreContext,
    pub logger: Logger,
    pub blobrepo: Arc<BlobRepo>,
    pub revlogrepo_path: PathBuf,
    pub changeset: Option<HgNodeHash>,
    pub skip: Option<usize>,
    pub commits_limit: Option<usize>,
    pub no_bookmark: bool,
}

impl Blobimport {
    pub fn import(self) -> BoxFuture<(), Error> {
        let Self {
            ctx,
            logger,
            blobrepo,
            revlogrepo_path,
            changeset,
            skip,
            commits_limit,
            no_bookmark,
        } = self;

        let stale_bookmarks = {
            let revlogrepo = RevlogRepo::open(&revlogrepo_path).expect("cannot open revlogrepo");
            bookmark::read_bookmarks(revlogrepo)
        };

        let revlogrepo = RevlogRepo::open(revlogrepo_path).expect("cannot open revlogrepo");

        let upload_changesets = UploadChangesets {
            ctx: ctx.clone(),
            blobrepo: blobrepo.clone(),
            revlogrepo: revlogrepo.clone(),
            changeset,
            skip,
            commits_limit,
        }.upload()
            .enumerate()
            .map({
                let logger = logger.clone();
                move |(cs_count, cs)| {
                    debug!(logger, "{} inserted: {}", cs_count, cs.1.get_changeset_id());
                    if cs_count % 5000 == 0 {
                        info!(logger, "inserted commits # {}", cs_count);
                    }
                    ()
                }
            })
            .map_err({
                let logger = logger.clone();
                move |err| {
                    error!(logger, "failed to blobimport: {}", err);

                    for cause in err.iter_chain() {
                        info!(logger, "cause: {}", cause);
                    }
                    info!(logger, "root cause: {:?}", err.find_root_cause());

                    let msg = format!("failed to blobimport: {}", err);
                    err_msg(msg)
                }
            })
            .for_each(|()| Ok(()))
            .inspect({
                let logger = logger.clone();
                move |()| {
                    info!(logger, "finished uploading changesets");
                }
            });

        stale_bookmarks
            .and_then(move |stale_bookmarks| upload_changesets.map(|()| stale_bookmarks))
            .and_then(move |stale_bookmarks| {
                if no_bookmark {
                    info!(
                        logger,
                        "since --no-bookmark was provided, bookmarks won't be imported"
                    );
                    future::ok(()).boxify()
                } else {
                    bookmark::upload_bookmarks(ctx, &logger, revlogrepo, blobrepo, stale_bookmarks)
                }
            })
            .boxify()
    }
}
