/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::error::SubcommandError;

use anyhow::{format_err, Error};
use blobrepo::BlobRepo;
use blobrepo_hg::BlobRepoHg;
use blobstore::Loadable;
use clap::{App, Arg, ArgMatches, SubCommand};
use cloned::cloned;
use cmdlib::{args, helpers};
use context::CoreContext;
use deleted_files_manifest::{find_entries, list_all_entries, RootDeletedManifestId};
use derived_data::BonsaiDerived;
use fbinit::FacebookInit;
use futures::{compat::Future01CompatExt, TryFutureExt, TryStreamExt};
use futures_ext::FutureExt;
use futures_old::{future::err, stream::futures_unordered, Future, IntoFuture, Stream};
use manifest::{get_implicit_deletes, PathOrPrefix};
use mercurial_types::HgManifestId;
use mononoke_types::{ChangesetId, MPath};
use revset::AncestorsNodeStream;
use slog::{debug, Logger};
use std::collections::BTreeSet;

pub const DELETED_MANIFEST: &str = "deleted-manifest";
const COMMAND_MANIFEST: &str = "manifest";
const COMMAND_VERIFY: &str = "verify";
const ARG_CSID: &str = "csid";
const ARG_LIMIT: &str = "limit";
const ARG_PATH: &str = "path";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    let csid_arg = Arg::with_name(ARG_CSID)
        .help("{hg|bonsai} changeset id or bookmark name")
        .index(1)
        .required(true);

    let path_arg = Arg::with_name(ARG_PATH)
        .help("path")
        .index(2)
        .default_value("");

    SubCommand::with_name(DELETED_MANIFEST)
        .about("derive, inspect and verify deleted files manifest")
        .subcommand(
            SubCommand::with_name(COMMAND_MANIFEST)
                .about("recursively list all deleted files manifest entries under the given path")
                .arg(csid_arg.clone())
                .arg(path_arg.clone()),
        )
        .subcommand(
            SubCommand::with_name(COMMAND_VERIFY)
                .about("verify deleted manifest against actual paths deleted in commits")
                .arg(csid_arg.clone())
                .arg(
                    Arg::with_name(ARG_LIMIT)
                        .help("number of commits to be verified")
                        .takes_value(true)
                        .required(true),
                ),
        )
}

pub async fn subcommand_deleted_manifest<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a ArgMatches<'_>,
    sub_matches: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    args::init_cachelib(fb, &matches, None);

    let repo = args::open_repo(fb, &logger, &matches).await?;
    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    match sub_matches.subcommand() {
        (COMMAND_MANIFEST, Some(matches)) => {
            let hash_or_bookmark = String::from(matches.value_of(ARG_CSID).unwrap());
            let path = match matches.value_of(ARG_PATH).unwrap() {
                "" => Ok(None),
                p => MPath::new(p).map(Some),
            };

            (Ok(repo), path)
                .into_future()
                .and_then(move |(repo, path)| {
                    helpers::csid_resolve(ctx.clone(), repo.clone(), hash_or_bookmark)
                        .and_then(move |cs_id| subcommand_manifest(ctx, repo, cs_id, path))
                })
                .from_err()
                .boxify()
        }
        (COMMAND_VERIFY, Some(matches)) => {
            let hash_or_bookmark = String::from(matches.value_of(ARG_CSID).unwrap());
            let limit = matches
                .value_of(ARG_LIMIT)
                .unwrap()
                .parse::<u64>()
                .expect("limit must be an integer");

            helpers::csid_resolve(ctx.clone(), repo.clone(), hash_or_bookmark)
                .and_then(move |cs_id| subcommand_verify(ctx, repo, cs_id, limit))
                .from_err()
                .boxify()
        }
        _ => err(SubcommandError::InvalidArgs).boxify(),
    }
    .compat()
    .await
}

fn subcommand_manifest(
    ctx: CoreContext,
    repo: BlobRepo,
    cs_id: ChangesetId,
    prefix: Option<MPath>,
) -> impl Future<Item = (), Error = Error> {
    RootDeletedManifestId::derive(ctx.clone(), repo.clone(), cs_id)
        .from_err()
        .and_then(move |root_manifest| {
            debug!(
                ctx.logger(),
                "ROOT Deleted Files Manifest {:?}", root_manifest,
            );

            let mf_id = root_manifest.deleted_manifest_id().clone();
            find_entries(
                ctx.clone(),
                repo.get_blobstore(),
                mf_id,
                Some(PathOrPrefix::Prefix(prefix)),
            )
            .collect()
        })
        .map(move |mut entries: Vec<_>| {
            entries.sort_by_key(|(path, _)| path.clone());
            for (path, mf_id) in entries {
                println!("{}/ {:?}", MPath::display_opt(path.as_ref()), mf_id);
            }
        })
}

fn subcommand_verify(
    ctx: CoreContext,
    repo: BlobRepo,
    cs_id: ChangesetId,
    limit: u64,
) -> impl Future<Item = (), Error = Error> {
    AncestorsNodeStream::new(ctx.clone(), &repo.get_changeset_fetcher(), cs_id)
        .take(limit)
        .for_each(move |cs_id| verify_single_commit(ctx.clone(), repo.clone(), cs_id))
}

fn get_parents(
    ctx: CoreContext,
    repo: BlobRepo,
    cs_id: ChangesetId,
) -> impl Future<Item = Vec<HgManifestId>, Error = Error> {
    cloned!(ctx, repo);
    repo.get_hg_from_bonsai_changeset(ctx.clone(), cs_id)
        .and_then({
            cloned!(ctx, repo);
            move |hg_cs_id| repo.get_changeset_parents(ctx.clone(), hg_cs_id)
        })
        .and_then({
            move |parent_hg_cs_ids| {
                cloned!(ctx, repo);
                let parents = parent_hg_cs_ids.into_iter().map(|cs_id| {
                    cs_id
                        .load(ctx.clone(), repo.blobstore())
                        .compat()
                        .from_err()
                        .map(move |blob_changeset| blob_changeset.manifestid().clone())
                });

                futures_unordered(parents).collect()
            }
        })
}

fn get_file_changes(
    ctx: CoreContext,
    repo: BlobRepo,
    cs_id: ChangesetId,
) -> impl Future<Item = (Vec<MPath>, Vec<MPath>), Error = Error> {
    let paths_added_fut = cs_id
        .load(ctx.clone(), &repo.get_blobstore())
        .compat()
        .from_err()
        .map(move |bonsai| {
            bonsai
                .into_mut()
                .file_changes
                .into_iter()
                .filter_map(|(path, change)| {
                    if let Some(_) = change {
                        Some(path)
                    } else {
                        None
                    }
                })
                .collect()
        });

    paths_added_fut
        .join(get_parents(ctx.clone(), repo.clone(), cs_id))
        .and_then(
            move |(paths_added, parent_manifests): (Vec<MPath>, Vec<HgManifestId>)| {
                get_implicit_deletes(
                    &ctx,
                    repo.get_blobstore(),
                    paths_added.clone(),
                    parent_manifests,
                )
                .compat()
                .collect()
                .map(move |paths_deleted| (paths_added, paths_deleted))
            },
        )
}

fn verify_single_commit(
    ctx: CoreContext,
    repo: BlobRepo,
    cs_id: ChangesetId,
) -> impl Future<Item = (), Error = Error> {
    let file_changes = get_file_changes(ctx.clone(), repo.clone(), cs_id.clone());
    let deleted_manifest_paths = RootDeletedManifestId::derive(ctx.clone(), repo.clone(), cs_id)
        .from_err()
        .and_then({
            cloned!(ctx, repo);
            move |root_manifest| {
                let mf_id = root_manifest.deleted_manifest_id().clone();
                list_all_entries(ctx.clone(), repo.get_blobstore(), mf_id).collect()
            }
        })
        .map(move |entries: Vec<_>| {
            entries
                .into_iter()
                .filter_map(move |(path_opt, ..)| path_opt)
                .collect::<BTreeSet<_>>()
        });

    file_changes.join(deleted_manifest_paths).and_then(
        move |((paths_added, paths_deleted), deleted_manifest_paths)| {
            for path in paths_added {
                // check that changed files are alive
                if deleted_manifest_paths.contains(&path) {
                    println!("Path {} is alive in changeset {:?}", path, cs_id);
                    return Err(format_err!("Path {} is alive", path));
                }
            }
            for path in paths_deleted {
                // check that deleted files are in the manifest
                if !deleted_manifest_paths.contains(&path) {
                    println!("Path {} was deleted in changeset {:?}", path, cs_id);
                    return Err(format_err!("Path {} is deleted", path));
                }
            }

            Ok(())
        },
    )
}
