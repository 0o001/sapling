/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::io::Write;
use std::sync::Arc;

use anyhow::bail;
use anyhow::format_err;
use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use blobstore::BlobstoreUnlinkOps;
use blobstore_factory::make_files_blobstore;
use blobstore_factory::make_manifold_blobstore;
use blobstore_factory::make_sql_blobstore;
use blobstore_factory::BlobstoreOptions;
use blobstore_factory::ReadOnlyStorage;
use cached_config::ConfigStore;
use clap::Parser;
use fbinit::FacebookInit;
use metaconfig_types::BlobConfig;
use metaconfig_types::BlobstoreId;
use metaconfig_types::StorageConfig;
use mononoke_app::args::AsRepoArg;
use mononoke_app::args::RepoArgs;
use mononoke_app::MononokeApp;
use BlobConfig::*;

/// Unlink blobstore keys
///
/// Currently only works for SqlBlob.
#[derive(Parser)]
pub struct CommandArgs {
    #[clap(flatten)]
    repo_args: RepoArgs,

    /// If the repo's blobstore is multiplexed, use this inner blobstore
    #[clap(long)]
    inner_blobstore_id: Option<u64>,

    /// Key of the blob to unlink
    key: String,
}

fn remove_wrapper_blobconfigs(mut blob_config: BlobConfig) -> BlobConfig {
    // Pack is a wrapper store - remove it
    while let BlobConfig::Pack { ref blobconfig, .. } = blob_config {
        blob_config = BlobConfig::clone(blobconfig);
    }
    blob_config
}

fn get_blobconfig(blob_config: BlobConfig, inner_blobstore_id: Option<u64>) -> Result<BlobConfig> {
    match inner_blobstore_id {
        None => Ok(blob_config),
        Some(inner_blobstore_id) => match blob_config {
            BlobConfig::MultiplexedWal { blobstores, .. } => {
                let seeked_id = BlobstoreId::new(inner_blobstore_id);
                blobstores
                    .into_iter()
                    .find_map(|(blobstore_id, _, blobstore)| {
                        if blobstore_id == seeked_id {
                            Some(remove_wrapper_blobconfigs(blobstore))
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        format_err!("could not find a blobstore with id {}", inner_blobstore_id)
                    })
            }
            _ => Err(format_err!(
                "inner-blobstore-id supplied but blobstore is not multiplexed"
            )),
        },
    }
}

async fn get_single_blobstore(
    fb: FacebookInit,
    storage_config: StorageConfig,
    inner_blobstore_id: u64,
    readonly_storage: ReadOnlyStorage,
    blobstore_options: &BlobstoreOptions,
    config_store: &ConfigStore,
) -> Result<Arc<dyn BlobstoreUnlinkOps>, Error> {
    let blobconfig = get_blobconfig(storage_config.blobstore, Some(inner_blobstore_id))?;

    let blobstore = match blobconfig {
        // Physical blobstores
        Sqlite { .. } | Mysql { .. } => make_sql_blobstore(
            fb,
            blobconfig,
            readonly_storage,
            blobstore_options,
            config_store,
        )
        .await
        .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?,
        Manifold { .. } | ManifoldWithTtl { .. } => {
            make_manifold_blobstore(fb, blobconfig, blobstore_options)
                .await
                .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?
        }
        Files { .. } => make_files_blobstore(blobconfig, blobstore_options)
            .await
            .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?,
        _ => {
            bail!(
                "Unlink is not implemented for this blobstore with inner_blobstore_id = {}",
                inner_blobstore_id
            )
        }
    };

    Ok(blobstore)
}

async fn get_multiple_blobstore(
    fb: FacebookInit,
    storage_config: StorageConfig,
    blobconfig: BlobConfig,
    readonly_storage: ReadOnlyStorage,
    blobstore_options: &BlobstoreOptions,
    config_store: &ConfigStore,
) -> Result<Vec<Arc<dyn BlobstoreUnlinkOps>>, Error> {
    let blobstores = match blobconfig {
        MultiplexedWal {
            multiplex_id: _,
            blobstores,
            write_quorum: _,
            queue_db: _,
            inner_blobstores_scuba_table: _,
            multiplex_scuba_table: _,
            scuba_sample_rate: _,
        } => {
            let mut underlying_blobstores: Vec<Arc<dyn BlobstoreUnlinkOps>> = Vec::new();
            writeln!(
                std::io::stdout(),
                "This MultiplexedWal blobstore has the following inner stores:"
            )?;
            for record in blobstores {
                let underlying_blobstore = get_single_blobstore(
                    fb,
                    storage_config.clone(),
                    record.0.into(),
                    readonly_storage,
                    blobstore_options,
                    config_store,
                )
                .await?;
                underlying_blobstores.push(underlying_blobstore);
                writeln!(std::io::stdout(), "Blobstore inner_id: {}", record.0)?;
            }
            underlying_blobstores
        }
        _ => {
            bail!("Only the MultiplexedWal type BlobConfig is allowd to pass into this funciton")
        }
    };
    Ok(blobstores)
}

async fn get_blobstore(
    fb: FacebookInit,
    storage_config: StorageConfig,
    inner_blobstore_id: Option<u64>,
    readonly_storage: ReadOnlyStorage,
    blobstore_options: &BlobstoreOptions,
    config_store: &ConfigStore,
) -> Result<Arc<dyn BlobstoreUnlinkOps>, Error> {
    let blobconfig = get_blobconfig(storage_config.blobstore, inner_blobstore_id)?;

    let blobstore = match blobconfig {
        // Physical blobstores
        Sqlite { .. } | Mysql { .. } => make_sql_blobstore(
            fb,
            blobconfig,
            readonly_storage,
            blobstore_options,
            config_store,
        )
        .await
        .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?,
        Manifold { .. } | ManifoldWithTtl { .. } => {
            make_manifold_blobstore(fb, blobconfig, blobstore_options)
                .await
                .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?
        }
        Files { .. } => make_files_blobstore(blobconfig, blobstore_options)
            .await
            .map(|store| Arc::new(store) as Arc<dyn BlobstoreUnlinkOps>)?,
        MultiplexedWal {
            multiplex_id: _,
            blobstores,
            write_quorum: _,
            queue_db: _,
            inner_blobstores_scuba_table: _,
            multiplex_scuba_table: _,
            scuba_sample_rate: _,
        } => {
            writeln!(
                std::io::stdout(),
                "This MultiplexedWal blobstore has the following inner stores:"
            )?;
            for record in blobstores {
                writeln!(std::io::stdout(), "Blobstore inner_id: {}", record.0)?;
            }
            bail!("Lets stop here. Next step is going to build a list of blobstores from these ids")
        }
        _ => {
            unimplemented!("This is implemented only for some blobstores.")
        }
    };

    Ok(blobstore)
}

pub async fn run(app: MononokeApp, args: CommandArgs) -> Result<()> {
    let ctx = app.new_basic_context();

    let repo_arg = args.repo_args.as_repo_arg();
    let (_repo_name, repo_config) = app.repo_config(repo_arg)?;
    let blobstore = get_blobstore(
        app.fb,
        repo_config.storage_config,
        args.inner_blobstore_id,
        app.environment().readonly_storage,
        &app.environment().blobstore_options,
        app.config_store(),
    )
    .await?;

    writeln!(std::io::stdout(), "Unlinking key {}", args.key)?;

    blobstore
        .unlink(&ctx, &args.key)
        .await
        .context("Failed to unlink blob")?;

    Ok(())
}
