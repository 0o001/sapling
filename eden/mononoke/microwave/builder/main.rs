/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

mod changesets;
mod filenodes;

use ::changesets::Changesets;
use ::filenodes::Filenodes;
use anyhow::{format_err, Error};
use blobrepo::BlobRepo;
use blobrepo_factory::{BlobrepoBuilder, PutBehaviour};
use blobrepo_override::DangerousOverride;
use bookmarks::BookmarkName;
use cache_warmup::{CacheWarmupRequest, CacheWarmupTarget};
use clap::{Arg, SubCommand};
use cloned::cloned;
use cmdlib::{
    args::{self, MononokeMatches},
    monitoring::AliveService,
};
use context::{CoreContext, SessionContainer};
use derived_data_filenodes::FilenodesOnlyPublic;
use fbinit::FacebookInit;
use futures::{channel::mpsc, future};
use mercurial_derived_data::MappedHgChangesetId;
use metaconfig_parser::RepoConfigs;
use metaconfig_types::CacheWarmupParams;
use microwave::{Snapshot, SnapshotLocation};
use slog::{info, o, Logger};
use std::path::Path;
use std::sync::Arc;
use warm_bookmarks_cache::{
    create_derived_data_warmer, find_all_underived_and_latest_derived, LatestDerivedBookmarkEntry,
};

use crate::changesets::MicrowaveChangesets;
use crate::filenodes::MicrowaveFilenodes;

const SUBCOMMAND_LOCAL_PATH: &str = "local-path";
const ARG_LOCAL_PATH: &str = "local-path";

const SUBCOMMAND_BLOBSTORE: &str = "blobstore";

async fn cache_warmup_target(
    ctx: &CoreContext,
    repo: &BlobRepo,
    bookmark: &BookmarkName,
) -> Result<CacheWarmupTarget, Error> {
    let warmers = vec![
        create_derived_data_warmer::<MappedHgChangesetId>(&ctx),
        create_derived_data_warmer::<FilenodesOnlyPublic>(&ctx),
    ];

    match find_all_underived_and_latest_derived(ctx, repo, bookmark, &warmers)
        .await?
        .0
    {
        LatestDerivedBookmarkEntry::Found(Some((cs_id, _))) => {
            Ok(CacheWarmupTarget::Changeset(cs_id))
        }
        LatestDerivedBookmarkEntry::Found(None) => {
            Err(format_err!("Bookmark {} has no derived data", bookmark))
        }
        LatestDerivedBookmarkEntry::NotFound => Err(format_err!(
            "Bookmark {} has too many underived commits",
            bookmark
        )),
    }
}

async fn do_main<'a>(
    fb: FacebookInit,
    matches: &MononokeMatches<'a>,
    logger: &Logger,
) -> Result<(), Error> {
    let mut scuba = args::get_scuba_sample_builder(fb, &matches)?;
    scuba.add_common_server_data();

    let mysql_options = cmdlib::args::parse_mysql_options(&matches);
    let readonly_storage = cmdlib::args::parse_readonly_storage(&matches);
    let blobstore_options = cmdlib::args::parse_blobstore_options(&matches);
    let caching = cmdlib::args::init_cachelib(fb, &matches);
    let config_store = cmdlib::args::init_config_store(fb, logger, matches)?;

    let RepoConfigs { repos, common } = args::load_repo_configs(config_store, &matches)?;
    let censored_scuba_params = common.censored_scuba_params;

    let location = match matches.subcommand() {
        (SUBCOMMAND_LOCAL_PATH, Some(sub)) => {
            let path = Path::new(sub.value_of_os(ARG_LOCAL_PATH).unwrap());
            info!(logger, "Writing to path {}", path.display());
            SnapshotLocation::SharedLocalPath(path)
        }
        (SUBCOMMAND_BLOBSTORE, Some(_)) => SnapshotLocation::Blobstore,
        (name, _) => return Err(format_err!("Invalid subcommand: {:?}", name)),
    };

    let futs = repos
        .into_iter()
        .map(|(name, config)| {
            cloned!(blobstore_options, censored_scuba_params, mut scuba);

            async move {
                let logger = logger.new(o!("repo" => name.clone()));

                let ctx = {
                    scuba.add("reponame", name.clone());
                    let session = SessionContainer::new_with_defaults(fb);
                    session.new_context(logger.clone(), scuba)
                };

                let (filenodes_sender, filenodes_receiver) = mpsc::channel(1000);
                let (changesets_sender, changesets_receiver) = mpsc::channel(1000);
                let warmup_ctx = ctx.clone();

                let warmup = async move {
                    let builder = BlobrepoBuilder::new(
                        fb,
                        name,
                        &config,
                        mysql_options,
                        caching,
                        censored_scuba_params,
                        readonly_storage,
                        blobstore_options,
                        &logger,
                        config_store,
                    );
                    let repo = builder.build().await?;

                    // Rewind bookmarks to the point where we have derived data. Cache
                    // warmup requires filenodes and hg changesets to be present.
                    let req = match config.cache_warmup {
                        Some(params) => {
                            let CacheWarmupParams {
                                bookmark,
                                commit_limit,
                                microwave_preload,
                            } = params;

                            let target = cache_warmup_target(&warmup_ctx, &repo, &bookmark).await?;

                            Some(CacheWarmupRequest {
                                target,
                                commit_limit,
                                microwave_preload,
                            })
                        }
                        None => None,
                    };

                    let repoid = config.repoid;
                    let warmup_repo = repo
                        .dangerous_override(|inner| -> Arc<dyn Filenodes> {
                            Arc::new(MicrowaveFilenodes::new(repoid, filenodes_sender, inner))
                        })
                        .dangerous_override(|inner| -> Arc<dyn Changesets> {
                            Arc::new(MicrowaveChangesets::new(repoid, changesets_sender, inner))
                        });

                    cache_warmup::cache_warmup(&warmup_ctx, &warmup_repo, req).await?;

                    Result::<_, Error>::Ok(repo)
                };

                let handle = tokio::task::spawn(warmup);
                let snapshot = Snapshot::build(filenodes_receiver, changesets_receiver).await;

                // Make sure cache warmup has succeeded before committing this snapshot, and get
                // the repo back.
                let repo = handle.await??;

                snapshot.commit(&ctx, &repo, location).await?;

                Result::<_, Error>::Ok(())
            }
        })
        .collect::<Vec<_>>();

    future::try_join_all(futs).await?;

    Ok(())
}

#[fbinit::main]
fn main(fb: FacebookInit) -> Result<(), Error> {
    let app = args::MononokeAppBuilder::new("Mononoke Local Replay")
        .with_advanced_args_hidden()
        .with_fb303_args()
        .with_all_repos()
        .with_scuba_logging_args()
        .with_special_put_behaviour(PutBehaviour::Overwrite)
        .build()
        .subcommand(
            SubCommand::with_name(SUBCOMMAND_LOCAL_PATH)
                .about("Write cache priming data to path")
                .arg(
                    Arg::with_name(ARG_LOCAL_PATH)
                        .takes_value(true)
                        .required(true),
                ),
        )
        .subcommand(
            SubCommand::with_name(SUBCOMMAND_BLOBSTORE)
                .about("Write cache priming data to the repository blobstore"),
        );

    let matches = app.get_matches();

    let logger = args::init_logging(fb, &matches);
    args::init_config_store(fb, &logger, &matches)?;

    let main = do_main(fb, &matches, &logger);

    cmdlib::helpers::block_execute(main, fb, "microwave", &logger, &matches, AliveService)?;

    Ok(())
}
