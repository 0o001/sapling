/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::{Context, Error, Result};
use blame::BlameRoot;
use blobrepo::BlobRepo;
use blobrepo_errors::*;
use blobstore::Blobstore;
use blobstore_factory::{make_blobstore, make_metadata_sql_factory, MetadataSqlFactory};
use bonsai_git_mapping::{BonsaiGitMapping, SqlBonsaiGitMappingConnection};
use bonsai_globalrev_mapping::{BonsaiGlobalrevMapping, SqlBonsaiGlobalrevMapping};
use bonsai_hg_mapping::{BonsaiHgMapping, CachingBonsaiHgMapping, SqlBonsaiHgMapping};
use bookmarks::{BookmarkUpdateLog, Bookmarks, CachedBookmarks};
use cacheblob::{
    new_cachelib_blobstore_no_lease, new_memcache_blobstore, CachelibBlobstoreOptions,
    InProcessLease, LeaseOps, MemcacheOps,
};
use cached_config::ConfigStore;
use changeset_fetcher::{ChangesetFetcher, SimpleChangesetFetcher};
use changeset_info::ChangesetInfo;
use changesets::{CachingChangesets, Changesets, SqlChangesets};
use dbbookmarks::SqlBookmarksBuilder;
use deleted_files_manifest::RootDeletedManifestId;
use derived_data::BonsaiDerived;
use derived_data_filenodes::FilenodesOnlyPublic;
use fastlog::RootFastlog;
use fbinit::FacebookInit;
use filenodes::Filenodes;
use filestore::FilestoreConfig;
use fsnodes::RootFsnodeId;
use futures::{compat::Future01CompatExt, future, try_join};
use git_types::TreeHandle;
use maplit::btreeset;
use memblob::Memblob;
use mercurial_derived_data::MappedHgChangesetId;
use mercurial_mutation::{HgMutationStore, SqlHgMutationStoreBuilder};
use metaconfig_types::{
    self, CensoredScubaParams, DerivedDataConfig, FilestoreParams, Redaction, RepoConfig,
    SegmentedChangelogConfig, StorageConfig, UnodeVersion,
};
use mononoke_types::RepositoryId;
use newfilenodes::NewFilenodesBuilder;
use phases::SqlPhasesFactory;
use readonlyblob::ReadOnlyBlobstore;
use redactedblobstore::{RedactedMetadata, SqlRedactedContentStore};
use repo_blobstore::RepoBlobstoreArgs;
use scuba_ext::MononokeScubaSampleBuilder;
use segmented_changelog::{
    DisabledSegmentedChangelog, SegmentedChangelog, SegmentedChangelogBuilder,
};
use skeleton_manifest::RootSkeletonManifestId;
use slog::Logger;
use sql::{rusqlite::Connection as SqliteConnection, Connection};
use sql_construct::SqlConstruct;
use sql_ext::{facebook::MysqlOptions, SqlConnections};
use std::num::NonZeroUsize;
use std::{collections::HashMap, sync::Arc, time::Duration};
use type_map::TypeMap;
use unodes::RootUnodeManifestId;
use virtually_sharded_blobstore::VirtuallyShardedBlobstore;

pub use blobstore_factory::{BlobstoreOptions, PutBehaviour, ReadOnlyStorage};

#[derive(Copy, Clone, PartialEq)]
pub enum Caching {
    // Usize in Enabled and CachelibOnlyBlobstore represents the number of cache shards. If zero,
    // sharding is not used.
    Enabled(usize),
    CachelibOnlyBlobstore(usize),
    Disabled,
}

const BLOBSTORE_BLOBS_CACHE_POOL: &'static str = "blobstore-blobs";
const BLOBSTORE_PRESENCE_CACHE_POOL: &'static str = "blobstore-presence";

pub struct BlobrepoBuilder<'a> {
    fb: FacebookInit,
    reponame: String,
    storage_config: StorageConfig,
    repoid: RepositoryId,
    mysql_options: MysqlOptions,
    caching: Caching,
    bookmarks_cache_ttl: Option<Duration>,
    redaction: Redaction,
    censored_scuba_params: CensoredScubaParams,
    filestore_params: Option<FilestoreParams>,
    readonly_storage: ReadOnlyStorage,
    blobstore_options: BlobstoreOptions,
    logger: &'a Logger,
    derived_data_config: DerivedDataConfig,
    segmented_changelog_config: SegmentedChangelogConfig,
    config_store: &'a ConfigStore,
}

impl<'a> BlobrepoBuilder<'a> {
    pub fn new(
        fb: FacebookInit,
        reponame: String,
        config: &RepoConfig,
        mysql_options: MysqlOptions,
        caching: Caching,
        censored_scuba_params: CensoredScubaParams,
        readonly_storage: ReadOnlyStorage,
        blobstore_options: BlobstoreOptions,
        logger: &'a Logger,
        config_store: &'a ConfigStore,
    ) -> Self {
        Self {
            fb,
            reponame,
            storage_config: config.storage_config.clone(),
            repoid: config.repoid,
            mysql_options,
            caching,
            bookmarks_cache_ttl: config.bookmarks_cache_ttl.clone(),
            redaction: config.redaction.clone(),
            censored_scuba_params,
            filestore_params: config.filestore.clone(),
            readonly_storage,
            blobstore_options,
            logger,
            derived_data_config: config.derived_data_config.clone(),
            segmented_changelog_config: config.segmented_changelog_config.clone(),
            config_store,
        }
    }

    pub fn set_redaction(&mut self, redaction: Redaction) {
        self.redaction = redaction;
    }

    /// remote (ie, MySQL), then it configures a full set of caches. Otherwise with local storage
    /// it's assumed to be a test configuration.
    ///
    /// The blobstore config is actually orthogonal to this, but it wouldn't make much sense to
    /// configure a local blobstore with a remote db, or vice versa. There's no error checking
    /// at this level (aside from disallowing a multiplexed blobstore with a local db).
    pub async fn build(self) -> Result<BlobRepo, Error> {
        let BlobrepoBuilder {
            fb,
            reponame,
            storage_config,
            repoid,
            mysql_options,
            caching,
            bookmarks_cache_ttl,
            redaction,
            censored_scuba_params,
            filestore_params,
            readonly_storage,
            blobstore_options,
            logger,
            derived_data_config,
            segmented_changelog_config,
            config_store,
        } = self;

        let sql_factory = make_metadata_sql_factory(
            fb,
            storage_config.metadata,
            mysql_options,
            readonly_storage,
            // FIXME: remove clone when make_metadata_sql_factory is async-await
            logger.clone(),
        )
        .compat();

        let blobstore = make_blobstore(
            fb,
            storage_config.blobstore,
            mysql_options,
            readonly_storage,
            &blobstore_options,
            &logger,
            config_store,
        );

        let (sql_factory, blobstore) = future::try_join(sql_factory, blobstore).await?;

        open_blobrepo_given_datasources(
            fb,
            blobstore,
            sql_factory,
            repoid,
            caching,
            bookmarks_cache_ttl,
            redaction,
            censored_scuba_params,
            filestore_params,
            readonly_storage,
            derived_data_config,
            segmented_changelog_config,
            reponame,
            blobstore_options.cachelib_options,
        )
        .await
    }
}

/// Expose for graph walker that has storage open already
pub async fn open_blobrepo_given_datasources(
    fb: FacebookInit,
    blobstore: Arc<dyn Blobstore>,
    sql_factory: MetadataSqlFactory,
    repoid: RepositoryId,
    caching: Caching,
    bookmarks_cache_ttl: Option<Duration>,
    redaction: Redaction,
    censored_scuba_params: CensoredScubaParams,
    filestore_params: Option<FilestoreParams>,
    readonly_storage: ReadOnlyStorage,
    derived_data_config: DerivedDataConfig,
    segmented_chagelog_config: SegmentedChangelogConfig,
    reponame: String,
    cachelib_options: CachelibBlobstoreOptions,
) -> Result<BlobRepo, Error> {
    let redacted_blobs = match redaction {
        Redaction::Enabled => {
            let redacted_blobs = sql_factory
                .open::<SqlRedactedContentStore>()
                .compat()
                .await?
                .get_all_redacted_blobs()
                .compat()
                .await?;
            Some(redacted_blobs)
        }
        Redaction::Disabled => None,
    };

    let filestore_config = filestore_params
        .map(|params| {
            let FilestoreParams {
                chunk_size,
                concurrency,
            } = params;

            FilestoreConfig {
                chunk_size: Some(chunk_size),
                concurrency,
            }
        })
        .unwrap_or_default();

    let repo = match caching {
        Caching::Disabled | Caching::CachelibOnlyBlobstore(_) => {
            let blobstore = if let Caching::CachelibOnlyBlobstore(cache_shards) = caching {
                get_cachelib_blobstore(blobstore, cache_shards, cachelib_options)?
            } else {
                blobstore
            };

            new_development(
                fb,
                &sql_factory,
                blobstore,
                redacted_blobs,
                censored_scuba_params,
                repoid,
                filestore_config,
                bookmarks_cache_ttl,
                derived_data_config,
                segmented_chagelog_config,
                reponame,
            )
            .await?
        }
        Caching::Enabled(cache_shards) => {
            let blobstore = new_memcache_blobstore(fb, blobstore, "multiplexed", "")?;
            let blobstore = get_cachelib_blobstore(blobstore, cache_shards, cachelib_options)?;

            new_production(
                fb,
                &sql_factory,
                blobstore,
                redacted_blobs,
                censored_scuba_params,
                repoid,
                bookmarks_cache_ttl,
                filestore_config,
                readonly_storage,
                derived_data_config,
                reponame,
            )
            .await?
        }
    };

    Ok(repo)
}

/// A helper to build test repositories.
pub struct TestRepoBuilder {
    repo_id: RepositoryId,
    blobstore: Arc<dyn Blobstore>,
    redacted: Option<HashMap<String, RedactedMetadata>>,
}

impl TestRepoBuilder {
    pub fn new() -> Self {
        Self {
            repo_id: RepositoryId::new(0),
            blobstore: Arc::new(Memblob::default()),
            redacted: None,
        }
    }

    pub fn id(mut self, repo_id: RepositoryId) -> Self {
        self.repo_id = repo_id;
        self
    }

    pub fn redacted(mut self, redacted: Option<HashMap<String, RedactedMetadata>>) -> Self {
        self.redacted = redacted;
        self
    }

    pub fn blobstore(mut self, blobstore: Arc<dyn Blobstore>) -> Self {
        self.blobstore = blobstore;
        self
    }

    fn maybe_blobstore(self, maybe_blobstore: Option<Arc<dyn Blobstore>>) -> Self {
        if let Some(blobstore) = maybe_blobstore {
            return self.blobstore(blobstore);
        }
        self
    }

    pub fn build(self) -> Result<BlobRepo> {
        let Self {
            repo_id,
            blobstore,
            redacted,
        } = self;

        let repo_blobstore_args = RepoBlobstoreArgs::new(
            blobstore,
            redacted,
            repo_id,
            MononokeScubaSampleBuilder::with_discard(),
        );

        let phases_factory = SqlPhasesFactory::with_sqlite_in_memory()?;

        let bookmarks =
            Arc::new(SqlBookmarksBuilder::with_sqlite_in_memory()?.with_repo_id(repo_id));

        let changesets = Arc::new(
            SqlChangesets::with_sqlite_in_memory()
                .context(ErrorKind::StateOpen(StateOpenError::Changesets))?,
        );
        let changeset_fetcher = Arc::new(SimpleChangesetFetcher::new(changesets.clone(), repo_id));

        Ok(blobrepo_new(
            bookmarks.clone(),
            bookmarks,
            repo_blobstore_args,
            Arc::new(
                NewFilenodesBuilder::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::Filenodes))?
                    .build(),
            ),
            changesets,
            changeset_fetcher,
            Arc::new(
                SqlBonsaiGitMappingConnection::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiGitMapping))?
                    .with_repo_id(repo_id),
            ),
            Arc::new(
                SqlBonsaiGlobalrevMapping::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiGlobalrevMapping))?,
            ),
            Arc::new(
                SqlBonsaiHgMapping::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::BonsaiHgMapping))?,
            ),
            Arc::new(
                SqlHgMutationStoreBuilder::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::HgMutationStore))?
                    .with_repo_id(repo_id),
            ),
            Arc::new(InProcessLease::new()),
            Arc::new(DisabledSegmentedChangelog::new()),
            FilestoreConfig::default(),
            phases_factory,
            init_all_derived_data(),
            "testrepo".to_string(),
        ))
    }
}

/// Used by tests
pub fn new_memblob_empty(blobstore: Option<Arc<dyn Blobstore>>) -> Result<BlobRepo> {
    TestRepoBuilder::new().maybe_blobstore(blobstore).build()
}

/// Used by cross-repo syncing tests
pub fn new_memblob_empty_with_id(
    blobstore: Option<Arc<dyn Blobstore>>,
    repo_id: RepositoryId,
) -> Result<BlobRepo> {
    TestRepoBuilder::new()
        .maybe_blobstore(blobstore)
        .id(repo_id)
        .build()
}

pub fn init_all_derived_data() -> DerivedDataConfig {
    DerivedDataConfig {
        scuba_table: None,
        derived_data_types: btreeset! {
            BlameRoot::NAME.to_string(),
            FilenodesOnlyPublic::NAME.to_string(),
            ChangesetInfo::NAME.to_string(),
            RootFastlog::NAME.to_string(),
            RootFsnodeId::NAME.to_string(),
            RootSkeletonManifestId::NAME.to_string(),
            RootDeletedManifestId::NAME.to_string(),
            RootUnodeManifestId::NAME.to_string(),
            TreeHandle::NAME.to_string(),
            MappedHgChangesetId::NAME.to_string(),
        },
        unode_version: UnodeVersion::V2,
        override_blame_filesize_limit: None,
    }
}

// Creates all db tables except for filenodes on the same sqlite connection
pub fn new_memblob_with_sqlite_connection_with_id(
    con: SqliteConnection,
    repo_id: RepositoryId,
) -> Result<(BlobRepo, Connection)> {
    con.execute_batch(SqlBookmarksBuilder::CREATION_QUERY)?;
    con.execute_batch(SqlChangesets::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiGitMappingConnection::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiGlobalrevMapping::CREATION_QUERY)?;
    con.execute_batch(SqlBonsaiHgMapping::CREATION_QUERY)?;
    con.execute_batch(SqlPhasesFactory::CREATION_QUERY)?;
    con.execute_batch(SqlHgMutationStoreBuilder::CREATION_QUERY)?;
    let con = Connection::with_sqlite(con);

    new_memblob_with_connection_with_id(con.clone(), repo_id)
}

pub fn new_memblob_with_connection_with_id(
    con: Connection,
    repo_id: RepositoryId,
) -> Result<(BlobRepo, Connection)> {
    let repo_blobstore_args = RepoBlobstoreArgs::new(
        Arc::new(Memblob::default()),
        None,
        repo_id,
        MononokeScubaSampleBuilder::with_discard(),
    );

    let sql_connections = SqlConnections::new_single(con.clone());

    let phases_factory = SqlPhasesFactory::from_sql_connections(sql_connections.clone());

    let bookmarks = Arc::new(
        SqlBookmarksBuilder::from_sql_connections(sql_connections.clone()).with_repo_id(repo_id),
    );
    let changesets = Arc::new(SqlChangesets::from_sql_connections(sql_connections.clone()));
    let changeset_fetcher = Arc::new(SimpleChangesetFetcher::new(changesets.clone(), repo_id));

    Ok((
        blobrepo_new(
            bookmarks.clone(),
            bookmarks,
            repo_blobstore_args,
            // Filenodes are intentionally created on another connection
            Arc::new(
                NewFilenodesBuilder::with_sqlite_in_memory()
                    .context(ErrorKind::StateOpen(StateOpenError::Filenodes))?
                    .build(),
            ),
            changesets,
            changeset_fetcher,
            Arc::new(
                SqlBonsaiGitMappingConnection::from_sql_connections(sql_connections.clone())
                    .with_repo_id(repo_id.clone()),
            ),
            Arc::new(SqlBonsaiGlobalrevMapping::from_sql_connections(
                sql_connections.clone(),
            )),
            Arc::new(SqlBonsaiHgMapping::from_sql_connections(
                sql_connections.clone(),
            )),
            Arc::new(
                SqlHgMutationStoreBuilder::from_sql_connections(sql_connections)
                    .with_repo_id(repo_id),
            ),
            Arc::new(InProcessLease::new()),
            Arc::new(DisabledSegmentedChangelog::new()),
            FilestoreConfig::default(),
            phases_factory,
            init_all_derived_data(),
            "testrepo".to_string(),
        ),
        con,
    ))
}

async fn new_development(
    fb: FacebookInit,
    sql_factory: &MetadataSqlFactory,
    blobstore: Arc<dyn Blobstore>,
    redacted_blobs: Option<HashMap<String, RedactedMetadata>>,
    censored_scuba_params: CensoredScubaParams,
    repoid: RepositoryId,
    filestore_config: FilestoreConfig,
    bookmarks_cache_ttl: Option<Duration>,
    derived_data_config: DerivedDataConfig,
    segmented_changelog_config: SegmentedChangelogConfig,
    reponame: String,
) -> Result<BlobRepo, Error> {
    let bookmarks = async {
        let sql_bookmarks = Arc::new(
            sql_factory
                .open::<SqlBookmarksBuilder>()
                .compat()
                .await
                .context(ErrorKind::StateOpen(StateOpenError::Bookmarks))?
                .with_repo_id(repoid),
        );

        let bookmarks: Arc<dyn Bookmarks> = if let Some(ttl) = bookmarks_cache_ttl {
            Arc::new(CachedBookmarks::new(sql_bookmarks.clone(), ttl, repoid))
        } else {
            sql_bookmarks.clone()
        };

        Ok((bookmarks, sql_bookmarks))
    };

    let filenodes_builder = async {
        sql_factory
            .open_shardable::<NewFilenodesBuilder>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::Filenodes))
    };

    let changesets = async {
        sql_factory
            .open::<SqlChangesets>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::Changesets))
    };

    let bonsai_git_mapping = async {
        let conn = sql_factory
            .open::<SqlBonsaiGitMappingConnection>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::BonsaiGitMapping))?;

        Ok(conn.with_repo_id(repoid))
    };

    let bonsai_globalrev_mapping = async {
        sql_factory
            .open::<SqlBonsaiGlobalrevMapping>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::BonsaiGlobalrevMapping))
    };

    let bonsai_hg_mapping = async {
        sql_factory
            .open::<SqlBonsaiHgMapping>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::BonsaiHgMapping))
    };

    let hg_mutation_store = async {
        let conn = sql_factory
            .open::<SqlHgMutationStoreBuilder>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::HgMutationStore))?;

        Ok(conn.with_repo_id(repoid))
    };

    let phases_factory = async {
        sql_factory
            .open::<SqlPhasesFactory>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::Phases))
    };

    let segmented_changelog_builder = async {
        sql_factory
            .open::<SegmentedChangelogBuilder>()
            .compat()
            .await
            .context(ErrorKind::StateOpen(StateOpenError::SegmentedChangelog))
    };

    let (
        (bookmarks, bookmark_update_log),
        filenodes_builder,
        changesets,
        bonsai_git_mapping,
        bonsai_globalrev_mapping,
        bonsai_hg_mapping,
        hg_mutation_store,
        phases_factory,
        segmented_changelog_builder,
    ) = try_join!(
        bookmarks,
        filenodes_builder,
        changesets,
        bonsai_git_mapping,
        bonsai_globalrev_mapping,
        bonsai_hg_mapping,
        hg_mutation_store,
        phases_factory,
        segmented_changelog_builder,
    )?;

    let censored_scuba_builder = get_censored_scuba_builder(fb, censored_scuba_params)?;
    let changesets = Arc::new(changesets);
    let changeset_fetcher = Arc::new(SimpleChangesetFetcher::new(changesets.clone(), repoid));
    let repo_blobstore_args =
        RepoBlobstoreArgs::new(blobstore, redacted_blobs, repoid, censored_scuba_builder);
    let segmented_changelog: Arc<dyn SegmentedChangelog> = if !segmented_changelog_config.enabled {
        Arc::new(segmented_changelog_builder.build_disabled())
    } else {
        if segmented_changelog_config.is_update_ondemand() {
            let dag = segmented_changelog_builder
                .with_repo_id(repoid)
                .with_changeset_fetcher(changeset_fetcher.clone())
                .build_on_demand_update()?;
            Arc::new(dag)
        } else if segmented_changelog_config.is_update_always_download_save() {
            let dag = segmented_changelog_builder
                .with_repo_id(repoid)
                .with_blobstore(Arc::new(repo_blobstore_args.repo_blobstore_clone()))
                .build_manager()?;
            Arc::new(dag)
        } else {
            let dag = segmented_changelog_builder
                .with_repo_id(repoid)
                .build_read_only()?;
            Arc::new(dag)
        }
    };

    Ok(blobrepo_new(
        bookmarks,
        bookmark_update_log,
        repo_blobstore_args,
        Arc::new(filenodes_builder.build()),
        changesets,
        changeset_fetcher,
        Arc::new(bonsai_git_mapping),
        Arc::new(bonsai_globalrev_mapping),
        Arc::new(bonsai_hg_mapping),
        Arc::new(hg_mutation_store),
        Arc::new(InProcessLease::new()),
        segmented_changelog,
        filestore_config,
        phases_factory,
        derived_data_config,
        reponame,
    ))
}

/// If the DB is remote then set up for a full production configuration.
/// In theory this could be with a local blobstore, but that would just be weird.
async fn new_production(
    fb: FacebookInit,
    sql_factory: &MetadataSqlFactory,
    blobstore: Arc<dyn Blobstore>,
    redacted_blobs: Option<HashMap<String, RedactedMetadata>>,
    censored_scuba_params: CensoredScubaParams,
    repoid: RepositoryId,
    bookmarks_cache_ttl: Option<Duration>,
    filestore_config: FilestoreConfig,
    readonly_storage: ReadOnlyStorage,
    derived_data_config: DerivedDataConfig,
    reponame: String,
) -> Result<BlobRepo, Error> {
    let filenodes_pool = get_volatile_pool("filenodes")?;
    let filenodes_history_pool = get_volatile_pool("filenodes_history")?;
    let changesets_cache_pool = get_volatile_pool("changesets")?;
    let bonsai_hg_mapping_cache_pool = get_volatile_pool("bonsai_hg_mapping")?;
    let phases_cache_pool = get_volatile_pool("phases")?;
    let derived_data_lease = MemcacheOps::new(fb, "derived-data-lease", "")?;

    let filenodes_tier = sql_factory.tier_name_shardable::<NewFilenodesBuilder>()?;
    let filenodes_builder = sql_factory.open_shardable::<NewFilenodesBuilder>().compat();
    let bookmarks = async {
        let builder = sql_factory.open::<SqlBookmarksBuilder>().compat().await?;

        Ok(builder.with_repo_id(repoid))
    };
    let changesets = sql_factory.open::<SqlChangesets>().compat();
    let bonsai_git_mapping = async {
        let conn = sql_factory
            .open::<SqlBonsaiGitMappingConnection>()
            .compat()
            .await?;

        Ok(conn.with_repo_id(repoid))
    };
    let bonsai_globalrev_mapping = sql_factory.open::<SqlBonsaiGlobalrevMapping>().compat();
    let bonsai_hg_mapping = sql_factory.open::<SqlBonsaiHgMapping>().compat();
    let hg_mutation_store = async {
        let conn = sql_factory
            .open::<SqlHgMutationStoreBuilder>()
            .compat()
            .await?;

        Ok(conn.with_repo_id(repoid))
    };
    let phases_factory = sql_factory.open::<SqlPhasesFactory>().compat();

    // Wrap again to avoid any writes to memcache
    let blobstore = if readonly_storage.0 {
        Arc::new(ReadOnlyBlobstore::new(blobstore)) as Arc<dyn Blobstore>
    } else {
        blobstore
    };

    let (
        mut filenodes_builder,
        mut phases_factory,
        bonsai_git_mapping,
        bookmarks,
        changesets,
        bonsai_globalrev_mapping,
        bonsai_hg_mapping,
        hg_mutation_store,
    ) = try_join!(
        filenodes_builder,
        phases_factory,
        bonsai_git_mapping,
        bookmarks,
        changesets,
        bonsai_globalrev_mapping,
        bonsai_hg_mapping,
        hg_mutation_store,
    )?;

    filenodes_builder.enable_caching(
        fb,
        filenodes_pool,
        filenodes_history_pool,
        "newfilenodes",
        &filenodes_tier,
    );

    let bookmarks = Arc::new(bookmarks);
    let (bookmarks, bookmark_update_log): (Arc<dyn Bookmarks>, Arc<dyn BookmarkUpdateLog>) =
        if let Some(ttl) = bookmarks_cache_ttl {
            (
                Arc::new(CachedBookmarks::new(bookmarks.clone(), ttl, repoid)),
                bookmarks,
            )
        } else {
            (bookmarks.clone(), bookmarks)
        };

    let changesets = Arc::new(CachingChangesets::new(
        fb,
        Arc::new(changesets),
        changesets_cache_pool,
    ));
    let changeset_fetcher = Arc::new(SimpleChangesetFetcher::new(changesets.clone(), repoid));

    let bonsai_hg_mapping = CachingBonsaiHgMapping::new(
        fb,
        Arc::new(bonsai_hg_mapping),
        bonsai_hg_mapping_cache_pool,
    );

    phases_factory.enable_caching(fb, phases_cache_pool);
    let censored_scuba_builder = get_censored_scuba_builder(fb, censored_scuba_params)?;

    Ok(blobrepo_new(
        bookmarks,
        bookmark_update_log,
        RepoBlobstoreArgs::new(blobstore, redacted_blobs, repoid, censored_scuba_builder),
        Arc::new(filenodes_builder.build()) as Arc<dyn Filenodes>,
        changesets,
        changeset_fetcher,
        Arc::new(bonsai_git_mapping),
        Arc::new(bonsai_globalrev_mapping),
        Arc::new(bonsai_hg_mapping),
        Arc::new(hg_mutation_store),
        Arc::new(derived_data_lease),
        Arc::new(DisabledSegmentedChangelog::new()),
        filestore_config,
        phases_factory,
        derived_data_config,
        reponame,
    ))
}

fn get_volatile_pool(name: &str) -> Result<cachelib::VolatileLruCachePool> {
    cachelib::get_volatile_pool(name)?
        .ok_or_else(|| Error::from(ErrorKind::MissingCachePool(name.to_string())))
}

fn get_cache_pool(name: &str) -> Result<cachelib::LruCachePool> {
    cachelib::get_pool(name)
        .ok_or_else(|| Error::from(ErrorKind::MissingCachePool(name.to_string())))
}

pub fn get_cachelib_blobstore<B: Blobstore + Clone + 'static>(
    blobstore: B,
    cache_shards: usize,
    options: CachelibBlobstoreOptions,
) -> Result<Arc<dyn Blobstore>, Error> {
    let blobstore = match NonZeroUsize::new(cache_shards) {
        Some(cache_shards) => {
            let blob_pool = get_volatile_pool(BLOBSTORE_BLOBS_CACHE_POOL)?;
            let presence_pool = get_volatile_pool(BLOBSTORE_PRESENCE_CACHE_POOL)?;

            Arc::new(VirtuallyShardedBlobstore::new(
                blobstore,
                blob_pool,
                presence_pool,
                // Semaphores are quite cheap compared to the size of the underlying cache.
                // This is at most a few MB.
                cache_shards,
                options,
            )) as Arc<dyn Blobstore>
        }
        None => {
            let blob_pool = get_cache_pool(BLOBSTORE_BLOBS_CACHE_POOL)?;
            let presence_pool = get_cache_pool(BLOBSTORE_PRESENCE_CACHE_POOL)?;

            Arc::new(new_cachelib_blobstore_no_lease(
                blobstore,
                Arc::new(blob_pool),
                Arc::new(presence_pool),
                options,
            )) as Arc<dyn Blobstore>
        }
    };

    Ok(blobstore)
}

pub fn blobrepo_new(
    bookmarks: Arc<dyn Bookmarks>,
    bookmark_update_log: Arc<dyn BookmarkUpdateLog>,
    blobstore_args: RepoBlobstoreArgs,
    filenodes: Arc<dyn Filenodes>,
    changesets: Arc<dyn Changesets>,
    changeset_fetcher: Arc<dyn ChangesetFetcher>,
    bonsai_git_mapping: Arc<dyn BonsaiGitMapping>,
    bonsai_globalrev_mapping: Arc<dyn BonsaiGlobalrevMapping>,
    bonsai_hg_mapping: Arc<dyn BonsaiHgMapping>,
    hg_mutation_store: Arc<dyn HgMutationStore>,
    derived_data_lease: Arc<dyn LeaseOps>,
    segmented_changelog: Arc<dyn SegmentedChangelog>,
    filestore_config: FilestoreConfig,
    phases_factory: SqlPhasesFactory,
    derived_data_config: DerivedDataConfig,
    reponame: String,
) -> BlobRepo {
    let attributes = {
        let mut attributes = TypeMap::new();
        attributes.insert::<dyn Bookmarks>(bookmarks);
        attributes.insert::<dyn BookmarkUpdateLog>(bookmark_update_log);
        attributes.insert::<dyn BonsaiHgMapping>(bonsai_hg_mapping);
        attributes.insert::<dyn Filenodes>(filenodes);
        attributes.insert::<dyn HgMutationStore>(hg_mutation_store);
        attributes.insert::<dyn ChangesetFetcher>(changeset_fetcher);
        attributes.insert::<dyn SegmentedChangelog>(segmented_changelog);
        Arc::new(attributes)
    };
    BlobRepo::new_dangerous(
        blobstore_args,
        changesets,
        bonsai_git_mapping,
        bonsai_globalrev_mapping,
        derived_data_lease,
        filestore_config,
        phases_factory,
        derived_data_config,
        reponame,
        attributes,
    )
}

fn get_censored_scuba_builder(
    fb: FacebookInit,
    censored_scuba_params: CensoredScubaParams,
) -> Result<MononokeScubaSampleBuilder, Error> {
    let mut builder = MononokeScubaSampleBuilder::with_opt_table(fb, censored_scuba_params.table);

    if let Some(scuba_log_file) = censored_scuba_params.local_path {
        builder = builder.with_log_file(scuba_log_file)?;
    }
    Ok(builder)
}
