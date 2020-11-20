/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use blobrepo::BlobRepo;
use blobstore::Loadable;
use bonsai_globalrev_mapping::{
    bulk_import_globalrevs, BonsaiGlobalrevMapping, SqlBonsaiGlobalrevMapping,
};
use bytes::Bytes;
use changesets::{deserialize_cs_entries, ChangesetEntry};
use clap::{App, Arg};
use cloned::cloned;
use cmdlib::{args, helpers::block_execute};
use context::CoreContext;
use fbinit::FacebookInit;
use futures::{compat::Future01CompatExt, future::try_join, FutureExt, TryFutureExt};
use futures_ext::{BoxFuture, FutureExt as _};
use futures_old::future::{Future, IntoFuture};
use futures_old::stream;
use futures_old::stream::Stream;
use std::fs;
use std::path::Path;
use std::sync::Arc;

fn setup_app<'a, 'b>() -> App<'a, 'b> {
    args::MononokeApp::new("Tool to upload globalrevs from commits saved in file")
        .build()
        .arg(Arg::from_usage(
            "<IN_FILENAME>  'file with bonsai changesets'",
        ))
}

fn parse_serialized_commits<P: AsRef<Path>>(file: P) -> Result<Vec<ChangesetEntry>, Error> {
    let data = fs::read(file).map_err(Error::from)?;
    deserialize_cs_entries(&Bytes::from(data))
}

pub fn upload<P: AsRef<Path>>(
    ctx: CoreContext,
    repo: BlobRepo,
    in_path: P,
    globalrevs_store: Arc<dyn BonsaiGlobalrevMapping>,
) -> BoxFuture<(), Error> {
    let chunk_size = 1000;
    parse_serialized_commits(in_path)
        .into_future()
        .and_then(move |changesets| {
            stream::iter_ok(changesets)
                .map({
                    cloned!(ctx, repo);
                    move |entry| {
                        cloned!(ctx, repo);
                        async move { entry.cs_id.load(ctx.clone(), repo.blobstore()).await }
                            .boxed()
                            .compat()
                            .from_err()
                    }
                })
                .buffered(chunk_size)
                .chunks(chunk_size)
                .and_then(move |chunk| {
                    bulk_import_globalrevs(
                        ctx.clone(),
                        repo.get_repoid(),
                        globalrevs_store.clone(),
                        chunk.iter(),
                    )
                })
                .for_each(|_| Ok(()))
        })
        .boxify()
}
#[fbinit::main]
fn main(fb: FacebookInit) -> Result<(), Error> {
    let matches = setup_app().get_matches();

    args::init_cachelib(fb, &matches, None);

    let logger = args::init_logging(fb, &matches);
    let config_store = args::init_config_store(fb, &logger, &matches)?;
    let ctx = CoreContext::new_with_logger(fb, logger.clone());
    let globalrevs_store = args::open_sql::<SqlBonsaiGlobalrevMapping>(fb, config_store, &matches);

    let blobrepo = args::open_repo(fb, &logger, &matches);
    let run = async {
        let (repo, globalrevs_store) = try_join(blobrepo, globalrevs_store).await?;
        let in_filename = matches.value_of("IN_FILENAME").unwrap();
        let globalrevs_store = Arc::new(globalrevs_store);
        upload(ctx, repo, in_filename, globalrevs_store)
            .compat()
            .await?;
        Ok(())
    };

    block_execute(
        run,
        fb,
        "upload_globalrevs",
        &logger,
        &matches,
        cmdlib::monitoring::AliveService,
    )
}
