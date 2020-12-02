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
use cmdlib::{
    args::{self, MononokeMatches},
    helpers,
};
use context::CoreContext;
use fbinit::FacebookInit;
use futures::compat::Future01CompatExt;
use manifest::{Entry, Manifest, ManifestOps};
use mercurial_types::{HgFileNodeId, HgManifestId, MPath};
use mononoke_types::FileType;
use slog::Logger;

use crate::error::SubcommandError;

pub const CONTENT_FETCH: &str = "content-fetch";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(CONTENT_FETCH)
        .about("fetches content of the file or manifest from blobrepo")
        .args_from_usage(
            "<CHANGESET_ID>    'hg/bonsai id or bookmark to fetch file from'
             <PATH>            'path to fetch'",
        )
}

pub async fn subcommand_content_fetch<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a MononokeMatches<'_>,
    sub_m: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    let rev = sub_m.value_of("CHANGESET_ID").unwrap().to_string();
    let path = sub_m.value_of("PATH").unwrap().to_string();

    args::init_cachelib(fb, &matches);

    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    let repo = args::open_repo(fb, &logger, &matches).await?;
    let entry = fetch_entry(&ctx, &repo, &rev, &path).await?;

    match entry {
        Entry::Leaf((FileType::Executable, _)) => {
            println!("Binary file");
        }
        Entry::Leaf((FileType::Symlink, id)) | Entry::Leaf((FileType::Regular, id)) => {
            let envelope = id.load(&ctx, repo.blobstore()).await.map_err(Error::from)?;
            let bytes =
                filestore::fetch_concat(&repo.get_blobstore(), &ctx, envelope.content_id()).await?;
            let content = String::from_utf8(bytes.to_vec()).expect("non-utf8 file content");
            println!("{}", content);
        }
        Entry::Tree(id) => {
            let manifest = id.load(&ctx, repo.blobstore()).await.map_err(Error::from)?;

            let entries: Vec<_> = manifest.list().collect();
            let mut longest_len = 0;
            for (name, _) in entries.iter() {
                let basename_len = name.len();
                if basename_len > longest_len {
                    longest_len = basename_len;
                }
            }

            for (name, entry) in entries {
                let mut name = String::from_utf8_lossy(name.as_ref()).to_string();
                for _ in name.len()..longest_len {
                    name.push(' ');
                }

                let (t, h) = match entry {
                    Entry::Leaf((t, id)) => (t.to_string(), id.to_string()),
                    Entry::Tree(id) => ("tree".to_string(), id.to_string()),
                };

                println!("{} {} {}", name, h, t);
            }
        }
    };

    Ok(())
}

async fn fetch_entry(
    ctx: &CoreContext,
    repo: &BlobRepo,
    rev: &str,
    path: &str,
) -> Result<Entry<HgManifestId, (FileType, HgFileNodeId)>, Error> {
    let mpath = MPath::new(path)?;

    let bcs_id = helpers::csid_resolve(ctx.clone(), repo.clone(), rev.to_string())
        .compat()
        .await?;
    let hg_cs_id = repo
        .get_hg_from_bonsai_changeset(ctx.clone(), bcs_id)
        .compat()
        .await?;
    let hg_cs = hg_cs_id.load(ctx, repo.blobstore()).await?;

    let ret = hg_cs
        .manifestid()
        .find_entry(ctx.clone(), repo.get_blobstore(), Some(mpath))
        .await?
        .ok_or_else(|| format_err!("Path does not exist: {}", path))?;

    Ok(ret)
}
