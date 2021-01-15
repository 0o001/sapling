/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{format_err, Error};
use blobrepo_hg::BlobRepoHg;
use bookmarks::Freshness;
use clap::{App, Arg, ArgMatches, SubCommand};
use cloned::cloned;
use context::CoreContext;
use futures::TryStreamExt;
use serde_json::{json, to_string_pretty};
use slog::{info, Logger};

use blobrepo::BlobRepo;
use bookmarks::{BookmarkName, BookmarkUpdateReason};

use crate::common::{fetch_bonsai_changeset, format_bookmark_log_entry};
use crate::error::SubcommandError;

pub const BOOKMARKS: &str = "bookmarks";
const SET_CMD: &str = "set";
const GET_CMD: &str = "get";
const LOG_CMD: &str = "log";
const LIST_CMD: &str = "list";
const DEL_CMD: &str = "delete";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    let parent_subcommand = SubCommand::with_name(BOOKMARKS);
    let set = SubCommand::with_name(SET_CMD)
        .about(
            "sets a bookmark to a specific hg changeset, if the bookmark does not exist it will
                be created",
        )
        .args_from_usage(
            "<BOOKMARK_NAME>        'bookmark to target'
             <HG_CHANGESET_ID>      'revision to which the bookmark should point to'",
        );

    let get = SubCommand::with_name(GET_CMD)
        .about("gets the changeset of a specific bookmark")
        .args_from_usage(
            r#"
            <BOOKMARK_NAME>        'bookmark to target'
            --json                 'if provided json will be returned'
            "#,
        )
        .arg(
            Arg::with_name("changeset-type")
                .long("changeset-type")
                .short("cs")
                .takes_value(true)
                .possible_values(&["bonsai", "hg"])
                .required(false)
                .help("What changeset type to return, either bonsai or hg. Defaults to hg."),
        );

    let log = SubCommand::with_name(LOG_CMD)
        .about("gets the log of changesets for a specific bookmark")
        .args_from_usage(
            r#"
            <BOOKMARK_NAME>        'bookmark to target'
            --json                 'if provided json will be returned'
            "#,
        )
        .arg(
            Arg::with_name("changeset-type")
                .long("changeset-type")
                .short("cs")
                .takes_value(true)
                .possible_values(&["bonsai", "hg"])
                .required(false)
                .help("What changeset type to return, either bonsai or hg. Defaults to hg."),
        )
        .arg(
            Arg::with_name("limit")
                .long("limit")
                .short("l")
                .takes_value(true)
                .required(false)
                .help("Imposes the limit on number of log records in output."),
        );

    let list = SubCommand::with_name(LIST_CMD).about("list bookmarks").arg(
        Arg::with_name("kind")
            .long("kind")
            .takes_value(true)
            .possible_values(&["publishing"])
            .required(true)
            .help("What set of bookmarks to list"),
    );

    let del = SubCommand::with_name(DEL_CMD)
        .about("delete bookmark")
        .args_from_usage(
            r#"
            <BOOKMARK_NAME>        'bookmark to delete'
            "#,
        );

    parent_subcommand
        .about("set of commands to manipulate bookmarks")
        .subcommand(set)
        .subcommand(get)
        .subcommand(log)
        .subcommand(list)
        .subcommand(del)
}

pub async fn handle_command(
    ctx: CoreContext,
    repo: BlobRepo,
    matches: &ArgMatches<'_>,
    _logger: Logger,
) -> Result<(), SubcommandError> {
    match matches.subcommand() {
        (GET_CMD, Some(sub_m)) => handle_get(sub_m, ctx, repo).await?,
        (SET_CMD, Some(sub_m)) => handle_set(sub_m, ctx, repo).await?,
        (LOG_CMD, Some(sub_m)) => handle_log(sub_m, ctx, repo).await?,
        (LIST_CMD, Some(sub_m)) => handle_list(sub_m, ctx, repo).await?,
        (DEL_CMD, Some(sub_m)) => handle_delete(sub_m, ctx, repo).await?,
        _ => return Err(SubcommandError::InvalidArgs),
    }
    Ok(())
}

fn format_output(json_flag: bool, changeset_id: String, changeset_type: &str) -> String {
    if json_flag {
        let answer = json!({
            "changeset_type": changeset_type,
            "changeset_id": changeset_id
        });
        to_string_pretty(&answer).unwrap()
    } else {
        format!("({}) {}", changeset_type.to_uppercase(), changeset_id)
    }
}

async fn handle_get(args: &ArgMatches<'_>, ctx: CoreContext, repo: BlobRepo) -> Result<(), Error> {
    let bookmark_name = args.value_of("BOOKMARK_NAME").unwrap().to_string();
    let bookmark = BookmarkName::new(bookmark_name).unwrap();
    let changeset_type = args.value_of("changeset-type").unwrap_or("hg");
    let json_flag: bool = args.is_present("json");

    match changeset_type {
        "hg" => {
            let cs = repo.get_bookmark(ctx, &bookmark).await?;
            let changeset_id_str = cs.expect("bookmark could not be found").to_string();
            let output = format_output(json_flag, changeset_id_str, "hg");
            println!("{}", output);
            Ok(())
        }
        "bonsai" => {
            let bonsai_cs =
                fetch_bonsai_changeset(ctx, bookmark.to_string().as_str(), &repo).await?;
            let changeset_id_str = bonsai_cs.get_changeset_id().to_string();
            let output = format_output(json_flag, changeset_id_str, "bonsai");
            println!("{}", output);
            Ok(())
        }
        _ => panic!("Unknown changeset-type supplied"),
    }
}

async fn handle_log(args: &ArgMatches<'_>, ctx: CoreContext, repo: BlobRepo) -> Result<(), Error> {
    let bookmark_name = args.value_of("BOOKMARK_NAME").unwrap().to_string();
    let bookmark = BookmarkName::new(bookmark_name).unwrap();
    let changeset_type = args.value_of("changeset-type").unwrap_or("hg");
    let json_flag = args.is_present("json");
    let output_limit_as_string = args.value_of("limit").unwrap_or("25");
    let max_rec = match output_limit_as_string.parse::<u32>() {
        Ok(n) => n,
        Err(e) => panic!(
            "Bad limit value supplied: \"{}\" - {}",
            output_limit_as_string, e
        ),
    };
    match changeset_type {
        "hg" => {
            repo.list_bookmark_log_entries(
                ctx.clone(),
                bookmark.clone(),
                max_rec,
                None,
                Freshness::MostRecent,
            )
            .map_ok({
                cloned!(ctx, repo);
                move |(entry_id, cs_id, rs, ts)| {
                    cloned!(ctx, repo);
                    async move {
                        match cs_id {
                            Some(cs_id) => {
                                let cs = repo
                                    .get_hg_from_bonsai_changeset(ctx.clone(), cs_id)
                                    .await?;
                                Ok((entry_id, Some(cs), rs, ts))
                            }
                            None => Ok((entry_id, None, rs, ts)),
                        }
                    }
                }
            })
            .try_buffer_unordered(100)
            .map_ok(move |rows| {
                let (entry_id, cs_id, reason, timestamp) = rows;
                let cs_id_str = match cs_id {
                    None => String::new(),
                    Some(x) => x.to_string(),
                };
                let output = format_bookmark_log_entry(
                    json_flag,
                    cs_id_str,
                    reason,
                    timestamp,
                    "hg",
                    bookmark.clone(),
                    Some(entry_id),
                );
                println!("{}", output);
            })
            .try_for_each(|_| async { Ok(()) })
            .await
        }
        "bonsai" => {
            repo.list_bookmark_log_entries(
                ctx,
                bookmark.clone(),
                max_rec,
                None,
                Freshness::MostRecent,
            )
            .map_ok(move |rows| {
                let (entry_id, cs_id, reason, timestamp) = rows;
                let cs_id_str = match cs_id {
                    None => String::new(),
                    Some(x) => x.to_string(),
                };
                let output = format_bookmark_log_entry(
                    json_flag,
                    cs_id_str,
                    reason,
                    timestamp,
                    "bonsai",
                    bookmark.clone(),
                    Some(entry_id),
                );
                println!("{}", output);
            })
            .try_for_each(|_| async { Ok(()) })
            .await
        }
        _ => panic!("Unknown changeset-type supplied"),
    }
}

async fn handle_list(args: &ArgMatches<'_>, ctx: CoreContext, repo: BlobRepo) -> Result<(), Error> {
    match args.value_of("kind") {
        Some("publishing") => {
            repo.get_bonsai_publishing_bookmarks_maybe_stale(ctx.clone())
                .try_for_each_concurrent(100, {
                    cloned!(repo, ctx);
                    move |(bookmark, bonsai_cs_id)| {
                        cloned!(ctx, repo);
                        async move {
                            let hg_cs_id = repo
                                .get_hg_from_bonsai_changeset(ctx.clone(), bonsai_cs_id)
                                .await?;
                            println!("{}\t{}\t{}", bookmark.into_name(), bonsai_cs_id, hg_cs_id);
                            Ok(())
                        }
                    }
                })
                .await
        }
        kind => panic!("Invalid kind {:?}", kind),
    }
}

async fn handle_set(args: &ArgMatches<'_>, ctx: CoreContext, repo: BlobRepo) -> Result<(), Error> {
    let bookmark_name = args.value_of("BOOKMARK_NAME").unwrap().to_string();
    let rev = args.value_of("HG_CHANGESET_ID").unwrap().to_string();
    let bookmark = BookmarkName::new(bookmark_name).unwrap();
    let new_bcs = fetch_bonsai_changeset(ctx.clone(), &rev, &repo).await?;
    let maybe_old_bcs_id = repo.get_bonsai_bookmark(ctx.clone(), &bookmark).await?;
    info!(
        ctx.logger(),
        "Current position of {:?} is {:?}", bookmark, maybe_old_bcs_id
    );
    let mut transaction = repo.update_bookmark_transaction(ctx);
    match maybe_old_bcs_id {
        Some(old_bcs_id) => {
            transaction.update(
                &bookmark,
                new_bcs.get_changeset_id(),
                old_bcs_id,
                BookmarkUpdateReason::ManualMove,
                None,
            )?;
        }
        None => {
            transaction.create(
                &bookmark,
                new_bcs.get_changeset_id(),
                BookmarkUpdateReason::ManualMove,
                None,
            )?;
        }
    }
    transaction.commit().await?;
    Ok(())
}

async fn handle_delete(
    args: &ArgMatches<'_>,
    ctx: CoreContext,
    repo: BlobRepo,
) -> Result<(), Error> {
    let bookmark_name = args.value_of("BOOKMARK_NAME").unwrap().to_string();
    let bookmark = BookmarkName::new(bookmark_name).unwrap();
    let maybe_bcs_id = repo.get_bonsai_bookmark(ctx.clone(), &bookmark).await?;
    info!(
        ctx.logger(),
        "Current position of {:?} is {:?}", bookmark, maybe_bcs_id
    );
    match maybe_bcs_id {
        Some(bcs_id) => {
            let mut transaction = repo.update_bookmark_transaction(ctx);
            transaction.delete(&bookmark, bcs_id, BookmarkUpdateReason::ManualMove, None)?;
            transaction.commit().await?;
            Ok(())
        }
        None => Err(format_err!("Cannot delete missing bookmark")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_output_format() {
        let expected_answer = json!({
            "changeset_type": "hg",
            "changeset_id": "123"
        });
        assert_eq!(
            format_output(true, "123".to_string(), "hg"),
            to_string_pretty(&expected_answer).unwrap()
        );
    }

    #[test]
    fn plain_output_format() {
        assert_eq!(format_output(false, "123".to_string(), "hg"), "(HG) 123");
    }
}
