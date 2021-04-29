/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use std::time::Duration;

use clap::Arg;
use criterion::{BenchmarkId, Criterion, Throughput};
use futures::{
    future::{self, TryFutureExt},
    stream::{Stream, StreamExt, TryStreamExt},
};
use tokio::runtime::Handle;

use bulkops::{Direction, PublicChangesetBulkFetch, MAX_FETCH_STEP};
use cmdlib::args;
use context::CoreContext;

const BENCHMARK_SAVE_BASELINE_ARG: &str = "benchmark-save-baseline";
const BENCHMARK_USE_BASELINE_ARG: &str = "benchmark-use-baseline";
const BENCHMARK_FILTER_ARG: &str = "benchmark-filter";

pub fn bench_stream<'a, F, S, O, E>(
    c: &'a mut Criterion,
    ctx: &'a CoreContext,
    runtime: &Handle,
    group: String,
    fetcher: &'a PublicChangesetBulkFetch,
    to_stream: F,
) where
    F: Fn(&'a CoreContext, &'a PublicChangesetBulkFetch) -> S,
    S: Stream<Item = Result<O, E>> + 'a,
    E: std::fmt::Debug,
{
    let mut group = c.benchmark_group(group);
    for num_to_load in [100000].iter() {
        group.throughput(Throughput::Elements(*num_to_load));
        for step in [MAX_FETCH_STEP].iter() {
            group.bench_with_input(
                BenchmarkId::from_parameter(step),
                num_to_load,
                |b, &num_to_load| {
                    let test = || async {
                        let mut loaded: u64 = 0;
                        let stream = to_stream(ctx, fetcher).take_while(|_entry| {
                            loaded += 1;
                            future::ready(loaded < num_to_load)
                        });
                        let _ = stream
                            .try_collect::<Vec<_>>()
                            .await
                            .expect("no stream errors");
                    };
                    b.iter(|| runtime.block_on(async { test().await }));
                },
            );
        }
    }
    group.finish();
}

#[fbinit::main]
fn main(fb: fbinit::FacebookInit) {
    let app = args::MononokeAppBuilder::new("benchmark_bulkops")
         .with_advanced_args_hidden()
         .build()
         .arg(
             Arg::with_name(BENCHMARK_SAVE_BASELINE_ARG)
                 .long(BENCHMARK_SAVE_BASELINE_ARG)
                 .takes_value(true)
                 .required(false)
                 .help("save results as a baseline under given name, for comparison"),
         )
         .arg(
             Arg::with_name(BENCHMARK_USE_BASELINE_ARG)
                 .long(BENCHMARK_USE_BASELINE_ARG)
                 .takes_value(true)
                 .required(false)
                 .conflicts_with(BENCHMARK_SAVE_BASELINE_ARG)
                 .help("compare to named baseline instead of last run"),
         )
         .arg(
             Arg::with_name(BENCHMARK_FILTER_ARG)
                 .long(BENCHMARK_FILTER_ARG)
                 .takes_value(true)
                 .required(false)
                 .multiple(true)
                 .help("limit to benchmarks whose name contains this string. Repetition tightens the filter"),
         );
    let matches = app.get_matches(fb).expect("Failed to start Mononoke");

    let mut criterion = Criterion::default()
        .measurement_time(Duration::from_secs(450))
        .sample_size(10)
        .warm_up_time(Duration::from_secs(60));

    if let Some(baseline) = matches.value_of(BENCHMARK_SAVE_BASELINE_ARG) {
        criterion = criterion.save_baseline(baseline.to_string());
    }
    if let Some(baseline) = matches.value_of(BENCHMARK_USE_BASELINE_ARG) {
        criterion = criterion.retain_baseline(baseline.to_string());
    }

    if let Some(filters) = matches.values_of(BENCHMARK_FILTER_ARG) {
        for filter in filters {
            criterion = criterion.with_filter(filter.to_string())
        }
    }

    let logger = matches.logger();
    let runtime = matches.runtime();
    let ctx = CoreContext::new_with_logger(fb, logger.clone());
    let blobrepo = args::open_repo(fb, &logger, &matches);

    let setup = {
        |runtime: &Handle| {
            runtime.block_on(async move {
                let blobrepo = blobrepo.await.expect("blobrepo should open");
                (
                    blobrepo.name().to_string(),
                    PublicChangesetBulkFetch::new(
                        blobrepo.get_repoid(),
                        blobrepo.get_changesets_object(),
                        blobrepo.get_phases(),
                    ),
                )
            })
        }
    };

    // Tests are run from here
    let (repo, fetcher) = setup(runtime);

    bench_stream(
        &mut criterion,
        &ctx,
        runtime,
        format!(
            "{}{}",
            repo, ":PublicChangesetBulkFetch::fetch_best_newest_first_mid"
        ),
        &fetcher,
        |ctx, fetcher| {
            async move {
                let (lower, upper) = fetcher.get_repo_bounds(ctx).await?;
                let mid = (upper - lower) / 2;
                Ok(fetcher.fetch_ids(ctx, Direction::NewestFirst, Some((lower, mid))))
            }
            .try_flatten_stream()
        },
    );

    bench_stream(
        &mut criterion,
        &ctx,
        runtime,
        format!(
            "{}{}",
            repo, ":PublicChangesetBulkFetch::fetch_best_oldest_first"
        ),
        &fetcher,
        |ctx, fetcher| fetcher.fetch_ids(ctx, Direction::OldestFirst, None),
    );

    bench_stream(
        &mut criterion,
        &ctx,
        runtime,
        format!(
            "{}{}",
            repo, ":PublicChangesetBulkFetch::fetch_entries_oldest_first"
        ),
        &fetcher,
        |ctx, fetcher| fetcher.fetch(ctx, Direction::OldestFirst),
    );

    criterion.final_summary();
}
