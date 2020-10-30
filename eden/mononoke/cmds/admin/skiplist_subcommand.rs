/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{anyhow, Error};
use bulkops::fetch_all_public_changesets;
use clap::{App, Arg, ArgMatches, SubCommand};
use fbinit::FacebookInit;
use fbthrift::compact_protocol;
use futures::{
    compat::{Future01CompatExt, Stream01CompatExt},
    future::{try_join, TryFutureExt},
    stream, StreamExt, TryStreamExt,
};
use futures_ext::{BoxFuture, FutureExt};
use futures_old::future::ok;
use futures_old::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

use blobrepo::BlobRepo;
use blobstore::Blobstore;
use changeset_fetcher::ChangesetFetcher;
use changesets::{ChangesetEntry, SqlChangesets};
use cmdlib::args;
use context::CoreContext;
use mononoke_types::{BlobstoreBytes, ChangesetId, Generation};
use skiplist::{deserialize_skiplist_index, sparse, SkiplistIndex, SkiplistNodeType};
use slog::{debug, info, Logger};
use std::num::NonZeroU64;

use crate::error::SubcommandError;

pub const SKIPLIST: &str = "skiplist";
const SKIPLIST_BUILD: &str = "build";
const ARG_SPARSE: &str = "sparse";
const SKIPLIST_READ: &str = "read";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(SKIPLIST)
        .about("commands to build or read skiplist indexes")
        .subcommand(
            SubCommand::with_name(SKIPLIST_BUILD)
                .about("build skiplist index")
                .arg(
                    Arg::with_name("BLOBSTORE_KEY")
                        .required(true)
                        .index(1)
                        .help("Blobstore key where to store the built skiplist"),
                )
                .arg(
                    Arg::with_name("rebuild")
                        .long("rebuild")
                        .help("forces the full rebuild instead of incremental update"),
                )
                .arg(
                    Arg::with_name(ARG_SPARSE)
                        .long(ARG_SPARSE)
                        .help("EXPERIMENTAL: build sparse skiplist. Makes skiplist smaller"),
                ),
        )
        .subcommand(
            SubCommand::with_name(SKIPLIST_READ)
                .about("read skiplist index")
                .arg(
                    Arg::with_name("BLOBSTORE_KEY")
                        .required(true)
                        .index(1)
                        .help("Blobstore key from where to read the skiplist"),
                ),
        )
}

#[derive(Copy, Clone)]
enum SkiplistType {
    Full,
    Sparse,
}

impl SkiplistType {
    fn new(sparse: bool) -> Self {
        if sparse {
            SkiplistType::Sparse
        } else {
            SkiplistType::Full
        }
    }
}

pub async fn subcommand_skiplist<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a ArgMatches<'_>,
    sub_m: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    match sub_m.subcommand() {
        (SKIPLIST_BUILD, Some(sub_m)) => {
            let key = sub_m
                .value_of("BLOBSTORE_KEY")
                .expect("blobstore key is not specified")
                .to_string();
            let rebuild = sub_m.is_present("rebuild");
            let skiplist_ty = SkiplistType::new(sub_m.is_present(ARG_SPARSE));

            args::init_cachelib(fb, &matches, None);
            let config_store = args::init_config_store(fb, &logger, matches)?;
            let ctx = CoreContext::new_with_logger(fb, logger.clone());
            let sql_changesets = args::open_sql::<SqlChangesets>(fb, config_store, &matches);
            let repo = args::open_repo(fb, &logger, &matches);
            let (repo, sql_changesets) = try_join(repo, sql_changesets).await?;
            build_skiplist_index(
                &ctx,
                &repo,
                key,
                &logger,
                &sql_changesets,
                rebuild,
                skiplist_ty,
            )
            .await
            .map_err(SubcommandError::Error)
        }
        (SKIPLIST_READ, Some(sub_m)) => {
            let key = sub_m
                .value_of("BLOBSTORE_KEY")
                .expect("blobstore key is not specified")
                .to_string();

            args::init_cachelib(fb, &matches, None);
            let ctx = CoreContext::test_mock(fb);
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let maybe_index = read_skiplist_index(ctx.clone(), repo, key, logger.clone())
                .compat()
                .await?;
            match maybe_index {
                Some(index) => {
                    info!(
                        logger,
                        "skiplist graph has {} entries",
                        index.indexed_node_count()
                    );
                }
                None => {
                    info!(logger, "skiplist not found");
                }
            }
            Ok(())
        }
        _ => Err(SubcommandError::InvalidArgs),
    }
}

async fn build_skiplist_index<'a, S: ToString>(
    ctx: &'a CoreContext,
    repo: &'a BlobRepo,
    key: S,
    logger: &'a Logger,
    sql_changesets: &'a SqlChangesets,
    force_full_rebuild: bool,
    skiplist_ty: SkiplistType,
) -> Result<(), Error> {
    let blobstore = repo.get_blobstore();
    // skiplist will jump up to 2^9 changesets
    let skiplist_depth = 10;
    // Index all changesets
    let max_index_depth = 20000000000;
    let key = key.to_string();
    let maybe_skiplist = if force_full_rebuild {
        None
    } else {
        read_skiplist_index(ctx.clone(), repo.clone(), key.clone(), logger.clone())
            .compat()
            .await?
    };

    let changeset_fetcher = repo.get_changeset_fetcher();
    let cs_fetcher_skiplist_func = async {
        match maybe_skiplist {
            Some(skiplist) => {
                info!(
                    logger,
                    "skiplist graph has {} entries",
                    skiplist.indexed_node_count()
                );
                Ok((changeset_fetcher, skiplist))
            }
            None => {
                info!(logger, "creating a skiplist from scratch");
                let skiplist_index = SkiplistIndex::with_skip_edge_count(skiplist_depth);
                let cs_fetcher = fetch_all_public_changesets_and_build_changeset_fetcher(
                    ctx,
                    repo,
                    sql_changesets,
                )
                .await?;
                Ok((cs_fetcher, skiplist_index))
            }
        }
    };

    let heads = repo
        .get_bonsai_heads_maybe_stale(ctx.clone())
        .compat()
        .try_collect::<Vec<_>>();

    let (heads, (cs_fetcher, skiplist_index)) = try_join(heads, cs_fetcher_skiplist_func).await?;

    let updated_skiplist = match skiplist_ty {
        SkiplistType::Full => {
            stream::iter(heads)
                .map(Ok)
                .try_for_each_concurrent(100, |head| {
                    skiplist_index.add_node(&ctx, &cs_fetcher, head, max_index_depth)
                })
                .await?;
            skiplist_index.get_all_skip_edges()
        }
        SkiplistType::Sparse => {
            let mut index = skiplist_index.get_all_skip_edges();
            let max_skip = NonZeroU64::new(2u64.pow(skiplist_depth - 1))
                .ok_or(anyhow!("invalid skiplist depth"))?;
            sparse::update_sparse_skiplist(&ctx, heads, &mut index, max_skip, &cs_fetcher).await?;
            index
        }
    };

    info!(logger, "build {} skiplist nodes", updated_skiplist.len());

    // We store only latest skip entry (i.e. entry with the longest jump)
    // This saves us storage space
    let mut thrift_merge_graph = HashMap::new();
    for (cs_id, skiplist_node_type) in updated_skiplist {
        let skiplist_node_type = if let SkiplistNodeType::SkipEdges(skip_edges) = skiplist_node_type
        {
            SkiplistNodeType::SkipEdges(skip_edges.last().cloned().into_iter().collect())
        } else {
            skiplist_node_type
        };

        thrift_merge_graph.insert(cs_id.into_thrift(), skiplist_node_type.to_thrift());
    }
    let bytes = compact_protocol::serialize(&thrift_merge_graph);

    debug!(logger, "storing {} bytes", bytes.len());
    blobstore
        .put(ctx.clone(), key, BlobstoreBytes::from_bytes(bytes))
        .await
}

async fn fetch_all_public_changesets_and_build_changeset_fetcher(
    ctx: &CoreContext,
    repo: &BlobRepo,
    sql_changesets: &SqlChangesets,
) -> Result<Arc<dyn ChangesetFetcher>, Error> {
    let fetched_changesets = fetch_all_public_changesets(
        &ctx,
        repo.get_repoid(),
        &sql_changesets,
        repo.get_phases().get_sql_phases(),
    )
    .try_collect::<Vec<_>>()
    .await?;

    let fetched_changesets: HashMap<_, _> = fetched_changesets
        .into_iter()
        .map(|cs_entry| (cs_entry.cs_id, cs_entry))
        .collect();
    let cs_fetcher: Arc<dyn ChangesetFetcher> = Arc::new(InMemoryChangesetFetcher {
        fetched_changesets: Arc::new(fetched_changesets),
        inner: repo.get_changeset_fetcher(),
    });

    Ok(cs_fetcher)
}

fn read_skiplist_index<S: ToString>(
    ctx: CoreContext,
    repo: BlobRepo,
    key: S,
    logger: Logger,
) -> BoxFuture<Option<SkiplistIndex>, Error> {
    repo.get_blobstore()
        .get(ctx, key.to_string())
        .compat()
        .and_then(move |maybebytes| {
            match maybebytes {
                Some(bytes) => {
                    debug!(
                        logger,
                        "received {} bytes from blobstore",
                        bytes.as_bytes().len()
                    );
                    let bytes = bytes.into_raw_bytes();
                    deserialize_skiplist_index(logger.clone(), bytes)
                        .into_future()
                        .map(Some)
                        .left_future()
                }
                None => ok(None).right_future(),
            }
        })
        .boxify()
}

#[derive(Clone)]
struct InMemoryChangesetFetcher {
    fetched_changesets: Arc<HashMap<ChangesetId, ChangesetEntry>>,
    inner: Arc<dyn ChangesetFetcher>,
}

impl ChangesetFetcher for InMemoryChangesetFetcher {
    fn get_generation_number(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> BoxFuture<Generation, Error> {
        match self.fetched_changesets.get(&cs_id) {
            Some(cs_entry) => ok(Generation::new(cs_entry.gen)).boxify(),
            None => self.inner.get_generation_number(ctx, cs_id),
        }
    }

    fn get_parents(
        &self,
        ctx: CoreContext,
        cs_id: ChangesetId,
    ) -> BoxFuture<Vec<ChangesetId>, Error> {
        match self.fetched_changesets.get(&cs_id) {
            Some(cs_entry) => ok(cs_entry.parents.clone()).boxify(),
            None => self.inner.get_parents(ctx, cs_id),
        }
    }
}
