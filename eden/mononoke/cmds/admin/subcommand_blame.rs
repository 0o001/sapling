/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::error::SubcommandError;

use anyhow::{format_err, Error};
use blame::{fetch_blame, fetch_file_full_content};
use blobrepo::BlobRepo;
use blobrepo_hg::BlobRepoHg;
use blobstore::Loadable;
use bytes::Bytes;
use clap::{App, Arg, ArgMatches, SubCommand};
use cloned::cloned;
use cmdlib::{args, helpers};
use context::CoreContext;
use derived_data::BonsaiDerived;
use fbinit::FacebookInit;
use futures::{
    compat::Future01CompatExt,
    future::{ready, try_join, FutureExt, TryFutureExt},
    StreamExt, TryStreamExt,
};
use futures_ext::{
    bounded_traversal::{bounded_traversal_dag, Iter},
    BoxFuture, FutureExt as OldFutureExt,
};
use futures_old::{future, Future, IntoFuture};
use manifest::ManifestOps;
use mononoke_types::{
    blame::{Blame, BlameMaybeRejected, BlameRejected},
    BlameId, ChangesetId, FileUnodeId, MPath,
};
use slog::Logger;
use std::collections::HashMap;
use std::fmt::Write;
use unodes::RootUnodeManifestId;

pub const BLAME: &str = "blame";
const COMMAND_DERIVE: &str = "derive";
const COMMAND_COMPUTE: &str = "compute";
const COMMAND_DIFF: &str = "diff";
const COMMAND_FIND_REJECTED: &str = "find-rejected";

const ARG_CSID: &str = "csid";
const ARG_PATH: &str = "path";
const ARG_PRINT_ERRORS: &str = "print-errors";
const ARG_LINE: &str = "line";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    let csid_arg = Arg::with_name(ARG_CSID)
        .help("{hg|bonsai} changeset id or bookmark name")
        .index(1)
        .required(true);
    let path_arg = Arg::with_name(ARG_PATH)
        .help("path")
        .index(2)
        .required(true);
    let line_number_arg = Arg::with_name(ARG_LINE)
        .help("show line number at the first appearance")
        .short("l")
        .long("line-number")
        .takes_value(false)
        .required(false);

    SubCommand::with_name(BLAME)
        .about("fetch/derive blame for specified changeset and path")
        .subcommand(
            SubCommand::with_name(COMMAND_DERIVE)
                .arg(line_number_arg.clone())
                .arg(csid_arg.clone())
                .arg(path_arg.clone()),
        )
        .subcommand(
            SubCommand::with_name(COMMAND_DIFF)
                .arg(csid_arg.clone())
                .arg(path_arg.clone()),
        )
        .subcommand(
            SubCommand::with_name(COMMAND_COMPUTE)
                .arg(line_number_arg.clone())
                .arg(csid_arg.clone())
                .arg(path_arg.clone()),
        )
        .subcommand(
            SubCommand::with_name(COMMAND_FIND_REJECTED)
                .arg(csid_arg.clone())
                .arg(
                    Arg::with_name(ARG_PRINT_ERRORS)
                        .help("print why the file is rejected")
                        .long(ARG_PRINT_ERRORS)
                        .takes_value(false)
                        .required(false),
                ),
        )
}

pub async fn subcommand_blame<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a ArgMatches<'_>,
    sub_matches: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    args::init_cachelib(fb, &matches, None);

    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    match sub_matches.subcommand() {
        (COMMAND_DERIVE, Some(matches)) => {
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let line_number = matches.is_present(ARG_LINE);
            with_changeset_and_path(ctx, repo, matches, move |ctx, repo, csid, path| {
                subcommand_show_blame(ctx, repo, csid, path, line_number)
            })
        }
        (COMMAND_DIFF, Some(matches)) => {
            let repo = args::open_repo(fb, &logger, &matches).await?;
            with_changeset_and_path(ctx, repo, matches, subcommand_show_diffs)
        }
        (COMMAND_COMPUTE, Some(matches)) => {
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let line_number = matches.is_present(ARG_LINE);
            with_changeset_and_path(ctx, repo, matches, move |ctx, repo, csid, path| {
                subcommand_compute_blame(ctx, repo, csid, path, line_number)
            })
        }
        (COMMAND_FIND_REJECTED, Some(matches)) => {
            let print_errors = matches.is_present(ARG_PRINT_ERRORS);
            let hash_or_bookmark = String::from(matches.value_of(ARG_CSID).unwrap());
            let repo = args::open_repo(fb, &logger, &matches).await?;
            let cs_id = helpers::csid_resolve(ctx.clone(), repo.clone(), hash_or_bookmark)
                .compat()
                .await?;

            let derived_unode = RootUnodeManifestId::derive(&ctx, &repo, cs_id)
                .map_err(Error::from)
                .await?;

            let mut paths = derived_unode
                .manifest_unode_id()
                .list_leaf_entries(ctx.clone(), repo.get_blobstore())
                .map_ok(|(path, file_unode_id)| {
                    BlameId::from(file_unode_id)
                        .load(ctx.clone(), repo.blobstore())
                        .map_ok(move |blame_maybe_rejected| (path, blame_maybe_rejected))
                        .map_err(Error::from)
                })
                .try_buffer_unordered(100)
                .try_filter_map(|(path, blame_maybe_rejected)| async move {
                    match blame_maybe_rejected {
                        BlameMaybeRejected::Rejected(rejected) => Ok(Some((path, rejected))),
                        BlameMaybeRejected::Blame(_) => Ok(None),
                    }
                })
                .boxed();

            while let Some((p, rejected)) = paths.try_next().await? {
                if print_errors {
                    println!("{} {}", p, rejected);
                } else {
                    println!("{}", p);
                }
            }
            return Result::<_, SubcommandError>::Ok(());
        }
        _ => future::err(SubcommandError::InvalidArgs).boxify(),
    }
    .compat()
    .await
}

fn with_changeset_and_path<F, FOut>(
    ctx: CoreContext,
    repo: BlobRepo,
    matches: &ArgMatches<'_>,
    fun: F,
) -> BoxFuture<(), SubcommandError>
where
    F: FnOnce(CoreContext, BlobRepo, ChangesetId, MPath) -> FOut + Send + 'static,
    FOut: Future<Item = (), Error = Error> + Send + 'static,
{
    let hash_or_bookmark = String::from(matches.value_of(ARG_CSID).unwrap());
    let path = MPath::new(matches.value_of(ARG_PATH).unwrap());
    (Ok(repo), path)
        .into_future()
        .and_then({
            move |(repo, path)| {
                helpers::csid_resolve(ctx.clone(), repo.clone(), hash_or_bookmark)
                    .and_then(move |csid| fun(ctx, repo, csid, path))
            }
        })
        .from_err()
        .boxify()
}

fn subcommand_show_blame(
    ctx: CoreContext,
    repo: BlobRepo,
    csid: ChangesetId,
    path: MPath,
    line_number: bool,
) -> impl Future<Item = (), Error = Error> {
    fetch_blame(ctx.clone(), repo.clone(), csid, path)
        .from_err()
        .and_then(move |(content, blame)| {
            blame_hg_annotate(ctx, repo, content, blame, line_number)
                .map(|annotate| println!("{}", annotate))
        })
}

fn find_leaf(
    ctx: CoreContext,
    repo: BlobRepo,
    csid: ChangesetId,
    path: MPath,
) -> impl Future<Item = FileUnodeId, Error = Error> {
    let blobstore = repo.get_blobstore();
    {
        cloned!(ctx, path);
        async move {
            let mf_root = RootUnodeManifestId::derive(&ctx, &repo, csid).await?;

            Ok(mf_root
                .manifest_unode_id()
                .clone()
                .find_entry(ctx, blobstore, Some(path))
                .await?)
        }
    }
    .boxed()
    .compat()
    .and_then({
        cloned!(path);
        move |entry_opt| {
            let entry = entry_opt.ok_or_else(|| format_err!("No such path: {}", path))?;
            match entry.into_leaf() {
                None => Err(format_err!(
                    "Blame is not available for directories: {}",
                    path
                )),
                Some(file_unode_id) => Ok(file_unode_id),
            }
        }
    })
}

fn subcommand_show_diffs(
    ctx: CoreContext,
    repo: BlobRepo,
    csid: ChangesetId,
    path: MPath,
) -> impl Future<Item = (), Error = Error> {
    let blobstore = repo.get_blobstore();
    find_leaf(ctx.clone(), repo.clone(), csid, path)
        .and_then({
            cloned!(ctx, blobstore);
            move |file_unode_id| {
                file_unode_id
                    .load(ctx, &blobstore)
                    .compat()
                    .from_err()
                    .map(move |file_unode| (file_unode_id, file_unode))
            }
        })
        .and_then(move |(file_unode_id, file_unode)| {
            let diffs = file_unode
                .parents()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .map({
                    cloned!(ctx, repo);
                    move |parent| diff(ctx.clone(), repo.clone(), file_unode_id, parent)
                });
            future::join_all(diffs).map(|diffs| {
                for diff in diffs {
                    println!("{}", diff);
                }
            })
        })
}

fn diff(
    ctx: CoreContext,
    repo: BlobRepo,
    new: FileUnodeId,
    old: FileUnodeId,
) -> impl Future<Item = String, Error = Error> {
    async move {
        let f1 = fetch_file_full_content(&ctx, &repo, new)
            .and_then(|result| ready(result.map_err(Error::from)));
        let f2 = fetch_file_full_content(&ctx, &repo, old)
            .and_then(|result| ready(result.map_err(Error::from)));
        try_join(f1, f2).await
    }
    .boxed()
    .compat()
    .map(|(new, old)| {
        let new = xdiff::DiffFile {
            path: "new",
            contents: xdiff::FileContent::Inline(new),
            file_type: xdiff::FileType::Regular,
        };
        let old = xdiff::DiffFile {
            path: "old",
            contents: xdiff::FileContent::Inline(old),
            file_type: xdiff::FileType::Regular,
        };
        let diff = xdiff::diff_unified(
            Some(old),
            Some(new),
            xdiff::DiffOpts {
                context: 3,
                copy_info: xdiff::CopyInfo::None,
            },
        );
        String::from_utf8_lossy(&diff).into_owned()
    })
}

/// Recalculate balme by going through whole history of a file
fn subcommand_compute_blame(
    ctx: CoreContext,
    repo: BlobRepo,
    csid: ChangesetId,
    path: MPath,
    line_number: bool,
) -> impl Future<Item = (), Error = Error> {
    let blobstore = repo.get_blobstore().boxed();
    find_leaf(ctx.clone(), repo.clone(), csid, path.clone())
        .and_then({
            cloned!(ctx, repo);
            move |file_unode_id| {
                bounded_traversal_dag(
                    256,
                    (file_unode_id, path),
                    {
                        // unfold operator traverses all parents of a given unode, accounting for
                        // renames and treating them as another parent.
                        cloned!(ctx, repo, blobstore);
                        move |(file_unode_id, path)| {
                            cloned!(ctx, repo, blobstore);
                            file_unode_id
                                .load(ctx.clone(), &blobstore)
                                .compat()
                                .from_err()
                                .and_then(move |file_unode| {
                                    let csid = *file_unode.linknode();
                                    csid.load(ctx.clone(), &blobstore)
                                        .compat()
                                        .from_err()
                                        .and_then({
                                            cloned!(ctx, repo, path);
                                            move |bonsai| {
                                                let copy_from = bonsai
                                                    .file_changes_map()
                                                    .get(&path)
                                                    .and_then(|file_change| file_change.as_ref())
                                                    .and_then(|file_change| {
                                                        file_change.copy_from().clone()
                                                    });
                                                match copy_from {
                                                    None => future::ok(None).left_future(),
                                                    Some((r_path, r_csid)) => find_leaf(
                                                        ctx,
                                                        repo,
                                                        *r_csid,
                                                        r_path.clone(),
                                                    )
                                                    .map({
                                                        cloned!(r_path);
                                                        move |r_unode_id| Some((r_unode_id, r_path))
                                                    })
                                                    .right_future(),
                                                }
                                            }
                                        })
                                        .map(move |copy_parent| {
                                            let parents: Vec<_> = file_unode
                                                .parents()
                                                .iter()
                                                .map(|unode_id| (*unode_id, path.clone()))
                                                .chain(copy_parent)
                                                .collect();
                                            ((csid, path, file_unode_id), parents)
                                        })
                                })
                        }
                    },
                    {
                        move |
                            (csid, path, file_unode_id),
                            parents: Iter<Result<(Bytes, Blame), BlameRejected>>,
                        | {
                            cloned!(path);
                            {
                                cloned!(ctx, repo);
                                async move {
                                    fetch_file_full_content(&ctx, &repo, file_unode_id).await
                                }
                            }
                            .boxed()
                            .compat()
                                .and_then(move |content| match content {
                                    Err(rejected) => Ok(Err(rejected)),
                                    Ok(content) => {
                                        let parents = parents
                                            .into_iter()
                                            .filter_map(|result| result.ok())
                                            .collect();
                                        Blame::from_parents(
                                            csid,
                                            content.clone(),
                                            path.clone(),
                                            parents,
                                        )
                                        .map(move |blame| Ok((content, blame)))
                                    }
                                })
                        }
                    },
                )
            }
        })
        .and_then(|result| Ok(result.ok_or_else(|| Error::msg("cycle found"))??))
        .and_then(move |(content, blame)| {
            blame_hg_annotate(ctx, repo, content, blame, line_number)
                .map(|annotate| println!("{}", annotate))
        })
}

/// Format blame the same way `hg blame` does
fn blame_hg_annotate<C: AsRef<[u8]> + 'static>(
    ctx: CoreContext,
    repo: BlobRepo,
    content: C,
    blame: Blame,
    show_line_number: bool,
) -> impl Future<Item = String, Error = Error> {
    if content.as_ref().is_empty() {
        return future::ok(String::new()).left_future();
    }

    let csids: Vec<_> = blame.ranges().iter().map(|range| range.csid).collect();
    repo.get_hg_bonsai_mapping(ctx, csids)
        .and_then(move |mapping| {
            let mapping: HashMap<_, _> = mapping.into_iter().map(|(k, v)| (v, k)).collect();

            let content = String::from_utf8_lossy(content.as_ref());
            let mut result = String::new();
            for (line, (csid, _path, line_number)) in content.lines().zip(blame.lines()) {
                let hg_csid = mapping
                    .get(&csid)
                    .ok_or_else(|| format_err!("unresolved bonsai csid: {}", csid))?;
                result.push_str(&hg_csid.to_string()[..12]);
                result.push_str(":");
                if show_line_number {
                    write!(&mut result, "{:>4}:", line_number + 1)?;
                }
                result.push_str(" ");
                result.push_str(line);
                result.push_str("\n");
            }

            Ok(result)
        })
        .right_future()
}
