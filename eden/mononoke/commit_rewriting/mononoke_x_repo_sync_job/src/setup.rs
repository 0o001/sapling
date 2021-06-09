/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Error};
use blobrepo::BlobRepo;
use clap::ArgMatches;
use cmdlib::{args::MononokeMatches, helpers};
use context::CoreContext;
use futures_old::{Future, IntoFuture};
use mononoke_types::ChangesetId;

use scuba_ext::MononokeScubaSampleBuilder;

use crate::cli::{ARG_COMMIT, ARG_LOG_TO_SCUBA, ARG_SLEEP_SECS};
use crate::reporting::SCUBA_TABLE;

const DEFAULT_SLEEP_SECS: u64 = 10;

pub fn get_starting_commit<'a>(
    ctx: CoreContext,
    matches: &ArgMatches<'a>,
    blobrepo: BlobRepo,
) -> impl Future<Item = ChangesetId, Error = Error> {
    matches
        .value_of(ARG_COMMIT)
        .ok_or_else(|| format_err!("{} argument is required", ARG_COMMIT))
        .map(|s| s.to_owned())
        .into_future()
        .and_then(move |str_value| helpers::csid_resolve(ctx, blobrepo, str_value))
}

pub fn get_scuba_sample<'a>(
    ctx: CoreContext,
    matches: &MononokeMatches<'a>,
) -> MononokeScubaSampleBuilder {
    let log_to_scuba = matches.is_present(ARG_LOG_TO_SCUBA);
    let mut scuba_sample = if log_to_scuba {
        MononokeScubaSampleBuilder::new(ctx.fb, SCUBA_TABLE)
    } else {
        MononokeScubaSampleBuilder::with_discard()
    };
    scuba_sample.add_common_server_data();
    scuba_sample
}

pub fn get_sleep_secs<'a>(matches: &ArgMatches<'a>) -> Result<u64, Error> {
    match matches.value_of(ARG_SLEEP_SECS) {
        Some(sleep_secs_str) => sleep_secs_str
            .parse::<u64>()
            .map_err(|_| format_err!("{} must be a valid u64", ARG_SLEEP_SECS)),
        None => Ok(DEFAULT_SLEEP_SECS),
    }
}
