/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Error};
use blobrepo::BlobRepo;
use blobrepo_hg::BlobRepoHg;
use blobstore::Loadable;
use clap::{App, ArgMatches, SubCommand};
use cmdlib::args::{self, MononokeMatches};
use context::CoreContext;
use fbinit::FacebookInit;
use futures::{compat::Stream01CompatExt, TryStreamExt};
use manifest::{bonsai_diff, BonsaiDiffFileChange};
use mercurial_types::{HgChangesetId, HgManifestId, MPath};
use revset::RangeNodeStream;
use serde_derive::Serialize;
use slog::Logger;
use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::io;
use std::str::FromStr;

use crate::error::SubcommandError;

pub const HG_CHANGESET: &str = "hg-changeset";
const HG_CHANGESET_DIFF: &str = "diff";
const HG_CHANGESET_RANGE: &str = "range";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(HG_CHANGESET)
        .about("mercural changeset level queries")
        .subcommand(
            SubCommand::with_name(HG_CHANGESET_DIFF)
                .about("compare two changeset (used by pushrebase replayer)")
                .args_from_usage(
                    "<LEFT_CS>  'left changeset id'
                     <RIGHT_CS> 'right changeset id'",
                ),
        )
        .subcommand(
            SubCommand::with_name(HG_CHANGESET_RANGE)
                .about("returns `x::y` revset")
                .args_from_usage(
                    "<START_CS> 'start changeset id'
                     <STOP_CS>  'stop changeset id'",
                ),
        )
}

pub async fn subcommand_hg_changeset<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a MononokeMatches<'_>,
    sub_m: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    match sub_m.subcommand() {
        (HG_CHANGESET_DIFF, Some(sub_m)) => {
            let left_cs = sub_m
                .value_of("LEFT_CS")
                .ok_or(format_err!("LEFT_CS argument expected"))
                .and_then(HgChangesetId::from_str)?;
            let right_cs = sub_m
                .value_of("RIGHT_CS")
                .ok_or(format_err!("RIGHT_CS argument expected"))
                .and_then(HgChangesetId::from_str)?;

            args::init_cachelib(fb, &matches);
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let diff = hg_changeset_diff(ctx, repo, left_cs, right_cs).await?;
            serde_json::to_writer(io::stdout(), &diff).map_err(Error::from)?;
            Ok(())
        }
        (HG_CHANGESET_RANGE, Some(sub_m)) => {
            let start_cs = sub_m
                .value_of("START_CS")
                .ok_or(format_err!("START_CS argument expected"))
                .and_then(HgChangesetId::from_str)?;
            let stop_cs = sub_m
                .value_of("STOP_CS")
                .ok_or(format_err!("STOP_CS argument expected"))
                .and_then(HgChangesetId::from_str)?;

            args::init_cachelib(fb, &matches);
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let (start_cs_opt, stop_cs_opt) = futures::try_join!(
                repo.get_bonsai_from_hg(ctx.clone(), start_cs),
                repo.get_bonsai_from_hg(ctx.clone(), stop_cs),
            )?;
            let start_cs = start_cs_opt.ok_or_else(|| Error::msg("failed to resolve changeset"))?;
            let stop_cs = stop_cs_opt.ok_or_else(|| Error::msg("failed to resovle changeset"))?;
            let css: Vec<_> =
                RangeNodeStream::new(ctx.clone(), repo.get_changeset_fetcher(), start_cs, stop_cs)
                    .compat()
                    .map_ok(|cs| repo.get_hg_from_bonsai_changeset(ctx.clone(), cs))
                    .try_buffer_unordered(100)
                    .map_ok(|cs| cs.to_hex().to_string())
                    .try_collect()
                    .await?;
            serde_json::to_writer(io::stdout(), &css).map_err(Error::from)?;
            Ok(())
        }
        _ => Err(SubcommandError::InvalidArgs),
    }
}

async fn hg_changeset_diff(
    ctx: CoreContext,
    repo: BlobRepo,
    left_id: HgChangesetId,
    right_id: HgChangesetId,
) -> Result<ChangesetDiff, Error> {
    let (left, right) = futures::try_join!(
        left_id.load(&ctx, repo.blobstore()),
        right_id.load(&ctx, repo.blobstore()),
    )?;

    let mut diff = ChangesetDiff {
        left: left_id,
        right: right_id,
        diff: Vec::new(),
    };

    if left.user() != right.user() {
        diff.diff.push(ChangesetAttrDiff::User(
            slice_to_str(left.user()),
            slice_to_str(right.user()),
        ));
    }

    if left.message() != right.message() {
        diff.diff.push(ChangesetAttrDiff::Comments(
            slice_to_str(left.message()),
            slice_to_str(right.message()),
        ))
    }

    if left.files() != right.files() {
        diff.diff.push(ChangesetAttrDiff::Files(
            left.files().iter().map(mpath_to_str).collect(),
            right.files().iter().map(mpath_to_str).collect(),
        ))
    }

    if left.extra() != right.extra() {
        diff.diff.push(ChangesetAttrDiff::Extra(
            left.extra()
                .iter()
                .map(|(k, v)| (slice_to_str(k), slice_to_str(v)))
                .collect(),
            right
                .extra()
                .iter()
                .map(|(k, v)| (slice_to_str(k), slice_to_str(v)))
                .collect(),
        ))
    }

    let mdiff = hg_manifest_diff(ctx, repo, left.manifestid(), right.manifestid()).await?;
    diff.diff.extend(mdiff);
    Ok(diff)
}

async fn hg_manifest_diff(
    ctx: CoreContext,
    repo: BlobRepo,
    left: HgManifestId,
    right: HgManifestId,
) -> Result<Option<ChangesetAttrDiff>, Error> {
    let diffs: Vec<_> = bonsai_diff(
        ctx,
        repo.get_blobstore(),
        left,
        Some(right).into_iter().collect(),
    )
    .try_collect()
    .await?;

    let diff = diffs.into_iter().fold(
        ManifestDiff {
            modified: Vec::new(),
            deleted: Vec::new(),
        },
        |mut mdiff, diff| {
            match diff {
                BonsaiDiffFileChange::Changed(path, ..)
                | BonsaiDiffFileChange::ChangedReusedId(path, ..) => {
                    mdiff.modified.push(mpath_to_str(path))
                }
                BonsaiDiffFileChange::Deleted(path) => mdiff.deleted.push(mpath_to_str(path)),
            };
            mdiff
        },
    );
    if diff.modified.is_empty() && diff.deleted.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ChangesetAttrDiff::Manifest(diff)))
    }
}

fn slice_to_str(slice: &[u8]) -> String {
    String::from_utf8_lossy(slice).into_owned()
}

fn mpath_to_str<P: Borrow<MPath>>(mpath: P) -> String {
    let bytes = mpath.borrow().to_vec();
    String::from_utf8_lossy(bytes.as_ref()).into_owned()
}

#[derive(Serialize)]
struct ChangesetDiff {
    left: HgChangesetId,
    right: HgChangesetId,
    diff: Vec<ChangesetAttrDiff>,
}

#[derive(Serialize)]
enum ChangesetAttrDiff {
    #[serde(rename = "user")]
    User(String, String),
    #[serde(rename = "comments")]
    Comments(String, String),
    #[serde(rename = "manifest")]
    Manifest(ManifestDiff),
    #[serde(rename = "files")]
    Files(Vec<String>, Vec<String>),
    #[serde(rename = "extra")]
    Extra(BTreeMap<String, String>, BTreeMap<String, String>),
}

#[derive(Serialize)]
struct ManifestDiff {
    modified: Vec<String>,
    deleted: Vec<String>,
}
