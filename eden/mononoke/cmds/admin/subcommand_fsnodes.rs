/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use crate::error::SubcommandError;

use anyhow::Error;
use blobrepo::BlobRepo;
use clap::{App, Arg, ArgMatches, SubCommand};
use cmdlib::{args, helpers};
use context::CoreContext;
use derived_data::BonsaiDerived;
use fbinit::FacebookInit;
use futures::{compat::Future01CompatExt, stream::StreamExt};
use manifest::{Entry, ManifestOps, PathOrPrefix};

use fsnodes::RootFsnodeId;
use mononoke_types::{ChangesetId, MPath};
use slog::{info, Logger};

pub const FSNODES: &str = "fsnodes";
const COMMAND_TREE: &str = "tree";
const ARG_CSID: &str = "csid";
const ARG_PATH: &str = "path";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(FSNODES)
        .about("inspect fsnodes")
        .subcommand(
            SubCommand::with_name(COMMAND_TREE)
                .about("recursively list all fsnode entries starting with prefix")
                .arg(
                    Arg::with_name(ARG_CSID)
                        .help("{hg|bonsai} changeset id or bookmark name")
                        .required(true),
                )
                .arg(Arg::with_name(ARG_PATH).help("path")),
        )
}

pub async fn subcommand_fsnodes<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a ArgMatches<'_>,
    sub_matches: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    args::init_cachelib(fb, &matches, None);

    let repo = args::open_repo(fb, &logger, &matches).await?;
    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    match sub_matches.subcommand() {
        (COMMAND_TREE, Some(matches)) => {
            let hash_or_bookmark = String::from(matches.value_of(ARG_CSID).unwrap());
            let path = matches.value_of(ARG_PATH).map(MPath::new).transpose()?;

            let csid = helpers::csid_resolve(ctx.clone(), repo.clone(), hash_or_bookmark)
                .compat()
                .await?;
            subcommand_tree(&ctx, &repo, csid, path).await?;
            Ok(())
        }
        _ => Err(SubcommandError::InvalidArgs),
    }
}

async fn subcommand_tree(
    ctx: &CoreContext,
    repo: &BlobRepo,
    csid: ChangesetId,
    path: Option<MPath>,
) -> Result<(), Error> {
    let root = RootFsnodeId::derive(ctx, repo, csid).await?;

    info!(ctx.logger(), "ROOT: {:?}", root);
    info!(ctx.logger(), "PATH: {:?}", path);

    let mut stream = root.fsnode_id().find_entries(
        ctx.clone(),
        repo.get_blobstore(),
        vec![PathOrPrefix::Prefix(path)],
    );

    while let Some((path, entry)) = stream.next().await.transpose()? {
        match entry {
            Entry::Tree(..) => {}
            Entry::Leaf(file) => {
                println!(
                    "{}\t{}\t{}\t{}",
                    MPath::display_opt(path.as_ref()),
                    file.content_id(),
                    file.file_type(),
                    file.size(),
                );
            }
        };
    }

    Ok(())
}
