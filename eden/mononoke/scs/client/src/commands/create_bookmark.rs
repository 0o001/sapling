/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use clap::{App, AppSettings, Arg, ArgMatches, SubCommand};
use futures::stream::{self, StreamExt};
use source_control::types as thrift;

use crate::args::commit_id::{add_commit_id_args, get_commit_id, resolve_commit_id};
use crate::args::pushvars::{add_pushvar_args, get_pushvars};
use crate::args::repo::{add_repo_args, get_repo_specifier};
use crate::args::service_id::{add_service_id_args, get_service_id};
use crate::connection::Connection;
use crate::render::RenderStream;

pub(super) const NAME: &str = "create-bookmark";

const ARG_NAME: &str = "BOOKMARK_NAME";

pub(super) fn make_subcommand<'a, 'b>() -> App<'a, 'b> {
    let cmd = SubCommand::with_name(NAME)
        .about("Create a bookmark")
        .setting(AppSettings::ColoredHelp);
    let cmd = add_repo_args(cmd);
    let cmd = add_commit_id_args(cmd);
    let cmd = add_service_id_args(cmd);
    let cmd = add_pushvar_args(cmd);
    cmd.arg(
        Arg::with_name(ARG_NAME)
            .short("n")
            .long("name")
            .takes_value(true)
            .help("Name of the bookmark to create")
            .required(true),
    )
}

pub(super) async fn run(matches: &ArgMatches<'_>, connection: Connection) -> Result<RenderStream> {
    let repo = get_repo_specifier(matches).expect("repository is required");
    let commit_id = get_commit_id(matches)?;
    let id = resolve_commit_id(&connection, &repo, &commit_id).await?;
    let bookmark = matches.value_of(ARG_NAME).expect("name is required").into();
    let service_identity = get_service_id(matches).map(String::from);
    let pushvars = get_pushvars(&matches)?;

    let params = thrift::RepoCreateBookmarkParams {
        bookmark,
        target: id,
        service_identity,
        pushvars,
    };
    connection.repo_create_bookmark(&repo, &params).await?;
    Ok(stream::empty().boxed())
}
