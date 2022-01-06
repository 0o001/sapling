/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{format_err, Context, Error};
use backsyncer::{open_backsyncer_dbs, TargetRepoDbs};
use blobrepo::BlobRepo;
use blobstore_factory::{make_blobstore, BlobstoreOptions, ReadOnlyStorage};
use cache_warmup::cache_warmup;
use cached_config::ConfigStore;
use cloned::cloned;
use context::CoreContext;
use fbinit::FacebookInit;
use metaconfig_types::{
    BackupRepoConfig, CommonCommitSyncConfig, RepoClientKnobs, WireprotoLoggingConfig,
};
use mononoke_api::Mononoke;
use mononoke_types::RepositoryId;
use repo_client::{MononokeRepo, PushRedirectorArgs, WireprotoLogging};
use scuba_ext::MononokeScubaSampleBuilder;
use slog::{debug, info, o, Logger};
use sql_construct::SqlConstructFromMetadataDatabaseConfig;
use sql_ext::facebook::MysqlOptions;

use synced_commit_mapping::SqlSyncedCommitMapping;

use crate::errors::ErrorKind;

/// An auxillary struct to pass between closures before we
/// are capable of creating a full `RepoHandler`
/// To create `RepoHandler`, we need to look at various
/// fields of such struct for other repos, so we first
/// have to construct all `IncompleteRepoHandler`s and
/// only then can we populate the `PushRedirector`
#[derive(Clone)]
struct IncompleteRepoHandler {
    logger: Logger,
    scuba: MononokeScubaSampleBuilder,
    wireproto_logging: Arc<WireprotoLogging>,
    repo: MononokeRepo,
    preserve_raw_bundle2: bool,
    maybe_incomplete_push_redirector_args: Option<IncompletePushRedirectorArgs>,
    repo_client_knobs: RepoClientKnobs,
    /// This is used for repositories that are backups of another prod repository
    backup_repo_config: Option<BackupRepoConfig>,
}

#[derive(Clone)]
struct IncompletePushRedirectorArgs {
    common_commit_sync_config: CommonCommitSyncConfig,
    synced_commit_mapping: SqlSyncedCommitMapping,
    target_repo_dbs: TargetRepoDbs,
    source_blobrepo: BlobRepo,
}

impl IncompletePushRedirectorArgs {
    fn try_into_push_redirector_args(
        self,
        repo_lookup_table: &HashMap<RepositoryId, IncompleteRepoHandler>,
    ) -> Result<PushRedirectorArgs, Error> {
        let Self {
            common_commit_sync_config,
            synced_commit_mapping,
            target_repo_dbs,
            source_blobrepo,
        } = self;

        let large_repo_id = common_commit_sync_config.large_repo_id;
        let target_repo: MononokeRepo = repo_lookup_table
            .get(&large_repo_id)
            .ok_or(ErrorKind::LargeRepoNotFound(large_repo_id))?
            .repo
            .clone();

        Ok(PushRedirectorArgs::new(
            target_repo,
            source_blobrepo,
            synced_commit_mapping,
            target_repo_dbs,
        ))
    }
}

impl IncompleteRepoHandler {
    fn try_into_repo_handler(
        self,
        repo_lookup_table: &HashMap<RepositoryId, IncompleteRepoHandler>,
    ) -> Result<RepoHandler, Error> {
        let IncompleteRepoHandler {
            logger,
            scuba,
            wireproto_logging,
            repo,
            preserve_raw_bundle2,
            maybe_incomplete_push_redirector_args,
            repo_client_knobs,
            backup_repo_config,
        } = self;

        let maybe_push_redirector_args = match maybe_incomplete_push_redirector_args {
            None => None,
            Some(incomplete_push_redirector_args) => Some(
                incomplete_push_redirector_args.try_into_push_redirector_args(repo_lookup_table)?,
            ),
        };

        let maybe_backup_repo_source = match backup_repo_config {
            None => None,
            Some(backup_repo_config) => {
                let backup_repo_source = try_find_repo_by_name(
                    &backup_repo_config.source_repo_name,
                    repo_lookup_table.values(),
                )?;
                Some(backup_repo_source)
            }
        };

        Ok(RepoHandler {
            logger,
            scuba,
            wireproto_logging,
            repo,
            preserve_raw_bundle2,
            maybe_push_redirector_args,
            repo_client_knobs,
            maybe_backup_repo_source,
        })
    }
}

fn try_find_repo_by_name<'a>(
    name: &str,
    iter: impl Iterator<Item = &'a IncompleteRepoHandler>,
) -> Result<BlobRepo, Error> {
    for handler in iter {
        let blobrepo = handler.repo.blobrepo();
        if blobrepo.name() == name {
            return Ok(blobrepo.clone());
        }
    }

    Err(format_err!("{} not found", name))
}

#[derive(Clone)]
pub struct RepoHandler {
    pub logger: Logger,
    pub scuba: MononokeScubaSampleBuilder,
    pub wireproto_logging: Arc<WireprotoLogging>,
    pub repo: MononokeRepo,
    pub preserve_raw_bundle2: bool,
    pub maybe_push_redirector_args: Option<PushRedirectorArgs>,
    pub repo_client_knobs: RepoClientKnobs,
    pub maybe_backup_repo_source: Option<BlobRepo>,
}

pub async fn repo_handlers<'a>(
    fb: FacebookInit,
    mononoke: &'a Mononoke,
    blobstore_options: &'a BlobstoreOptions,
    mysql_options: &'a MysqlOptions,
    readonly_storage: ReadOnlyStorage,
    root_log: &Logger,
    config_store: &'a ConfigStore,
    scuba: &MononokeScubaSampleBuilder,
) -> Result<HashMap<String, RepoHandler>, Error> {
    let futs = mononoke.repos().map(|repo| async move {
        let reponame = repo.name().clone();
        let config = repo.config();

        let root_log = root_log.clone();

        let logger = root_log.new(o!("repo" => reponame.clone()));
        let ctx = CoreContext::new_with_logger(fb, logger.clone());


        // Clone the few things we're going to need later in our bootstrap.
        let cache_warmup_params = config.cache_warmup.clone();
        let db_config = config.storage_config.metadata.clone();
        let preserve_raw_bundle2 = config.bundle2_replay_params.preserve_raw_bundle2.clone();
        let wireproto_logging = config.wireproto_logging.clone();

        let common_commit_sync_config = repo
            .live_commit_sync_config()
            .get_common_config_if_exists(repo.blob_repo().get_repoid())?;

        let repo_client_knobs = config.repo_client_knobs.clone();
        let backup_repo_config = config.backup_repo_config.clone();

        let blobrepo = repo.blob_repo().clone();

        info!(logger, "Warming up cache");
        let initial_warmup = tokio::task::spawn({
            cloned!(ctx, blobrepo, reponame);
            async move {
                cache_warmup(&ctx, &blobrepo, cache_warmup_params)
                    .await
                    .with_context(|| format!("while warming up cache for repo: {}", reponame))
            }
        });

        let sql_commit_sync_mapping = SqlSyncedCommitMapping::with_metadata_database_config(
            fb,
            &db_config,
            mysql_options,
            readonly_storage.0,
        )?;

        let wireproto_logging = create_wireproto_logging(
            fb,
            reponame.clone(),
            blobstore_options,
            mysql_options,
            readonly_storage,
            wireproto_logging,
            logger.clone(),
            config_store,
        );

        let backsyncer_dbs = open_backsyncer_dbs(
            ctx.clone(),
            blobrepo.clone(),
            db_config.clone(),
            mysql_options.clone(),
            readonly_storage,
        );

        info!(
            logger,
            "Creating MononokeRepo, CommitSyncMapping, WireprotoLogging, TargetRepoDbs, \
                WarmBookmarksCache"
        );

        let mononoke_repo =
            MononokeRepo::new(ctx.fb, repo.clone(), mysql_options, readonly_storage);

        let (mononoke_repo, wireproto_logging, backsyncer_dbs) =
            futures::future::try_join3(mononoke_repo, wireproto_logging, backsyncer_dbs).await?;

        let maybe_incomplete_push_redirector_args = common_commit_sync_config.and_then({
            cloned!(logger);
            move |common_commit_sync_config| {
                if common_commit_sync_config.large_repo_id == blobrepo.get_repoid() {
                    debug!(
                        logger,
                        "Not constructing push redirection args: {:?}",
                        blobrepo.get_repoid()
                    );
                    None
                } else {
                    debug!(
                        logger,
                        "Constructing incomplete push redirection args: {:?}",
                        blobrepo.get_repoid()
                    );
                    Some(IncompletePushRedirectorArgs {
                        common_commit_sync_config,
                        synced_commit_mapping: sql_commit_sync_mapping,
                        target_repo_dbs: backsyncer_dbs,
                        source_blobrepo: blobrepo,
                    })
                }
            }
        });

        initial_warmup.await??;

        info!(logger, "Repository is ready");
        Result::<_, Error>::Ok((
            reponame,
            IncompleteRepoHandler {
                logger,
                scuba: scuba.clone(),
                wireproto_logging: Arc::new(wireproto_logging),
                repo: mononoke_repo,
                preserve_raw_bundle2,
                maybe_incomplete_push_redirector_args,
                repo_client_knobs,
                backup_repo_config,
            },
        ))
    });

    let tuples = futures::future::try_join_all(futs).await?;

    build_repo_handlers(tuples).await
}

async fn build_repo_handlers(
    tuples: Vec<(String, IncompleteRepoHandler)>,
) -> Result<HashMap<String, RepoHandler>, Error> {
    let lookup_table: HashMap<RepositoryId, IncompleteRepoHandler> = tuples
        .iter()
        .map(|(_, incomplete_repo_handler)| {
            (
                incomplete_repo_handler.repo.repoid(),
                incomplete_repo_handler.clone(),
            )
        })
        .collect();

    let mut res = HashMap::new();
    for (reponame, incomplete_repo_handler) in tuples {
        let repo_handler = incomplete_repo_handler.try_into_repo_handler(&lookup_table)?;
        res.insert(reponame, repo_handler);
    }
    Ok(res)
}

async fn create_wireproto_logging<'a>(
    fb: FacebookInit,
    reponame: String,
    blobstore_options: &'a BlobstoreOptions,
    mysql_options: &'a MysqlOptions,
    readonly_storage: ReadOnlyStorage,
    wireproto_logging_config: WireprotoLoggingConfig,
    logger: Logger,
    config_store: &'a ConfigStore,
) -> Result<WireprotoLogging, Error> {
    let WireprotoLoggingConfig {
        storage_config_and_threshold,
        scribe_category,
        local_path,
    } = wireproto_logging_config;
    let blobstore_and_threshold = match storage_config_and_threshold {
        Some((storage_config, threshold)) => {
            if readonly_storage.0 {
                return Err(format_err!(
                    "failed to create blobstore for wireproto logging because storage is readonly",
                ));
            }

            let blobstore = make_blobstore(
                fb,
                storage_config.blobstore,
                mysql_options,
                readonly_storage,
                blobstore_options,
                &logger,
                config_store,
                &blobstore_factory::default_scrub_handler(),
                None,
            )
            .await?;

            Some((blobstore, threshold))
        }
        None => None,
    };

    WireprotoLogging::new(
        fb,
        reponame,
        scribe_category,
        blobstore_and_threshold,
        local_path.as_ref().map(|p| p.as_ref()),
    )
}
