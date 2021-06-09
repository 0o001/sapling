/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Repository factory.
#![feature(trait_alias)]

use skiplist::{ArcSkiplistIndex, SkiplistIndex};
use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_once_cell::AsyncOnceCell;
use blobstore::Blobstore;
use blobstore_factory::{
    default_scrub_handler, make_blobstore, make_metadata_sql_factory, ComponentSamplingHandler,
    MetadataSqlFactory, ScrubHandler,
};
use bonsai_git_mapping::{ArcBonsaiGitMapping, SqlBonsaiGitMappingConnection};
use bonsai_globalrev_mapping::{
    ArcBonsaiGlobalrevMapping, CachingBonsaiGlobalrevMapping, SqlBonsaiGlobalrevMapping,
};
use bonsai_hg_mapping::{ArcBonsaiHgMapping, CachingBonsaiHgMapping, SqlBonsaiHgMappingBuilder};
use bonsai_svnrev_mapping::{
    ArcRepoBonsaiSvnrevMapping, BonsaiSvnrevMapping, CachingBonsaiSvnrevMapping,
    RepoBonsaiSvnrevMapping, SqlBonsaiSvnrevMapping,
};
use bookmarks::{ArcBookmarkUpdateLog, ArcBookmarks, CachedBookmarks};
use cacheblob::{
    new_cachelib_blobstore_no_lease, new_memcache_blobstore, CachelibBlobstoreOptions,
    InProcessLease, LeaseOps, MemcacheOps,
};
use changeset_fetcher::{ArcChangesetFetcher, SimpleChangesetFetcher};
use changesets::ArcChangesets;
use changesets_impl::{CachingChangesets, SqlChangesetsBuilder};
use context::SessionContainer;
use dbbookmarks::{ArcSqlBookmarks, SqlBookmarksBuilder};
use environment::{Caching, MononokeEnvironment};
use filenodes::ArcFilenodes;
use filestore::{ArcFilestoreConfig, FilestoreConfig};
use futures_watchdog::WatchdogExt;
use mercurial_mutation::{ArcHgMutationStore, SqlHgMutationStoreBuilder};
use metaconfig_types::{
    ArcRepoConfig, BlobConfig, CensoredScubaParams, MetadataDatabaseConfig, Redaction, RepoConfig,
};
use newfilenodes::NewFilenodesBuilder;
use parking_lot::Mutex;
use phases::{ArcSqlPhasesFactory, SqlPhasesFactory};
use pushrebase_mutation_mapping::{
    ArcPushrebaseMutationMapping, SqlPushrebaseMutationMappingConnection,
};
use readonlyblob::ReadOnlyBlobstore;
use redactedblobstore::{RedactedMetadata, SqlRedactedContentStore};
use repo_blobstore::{ArcRepoBlobstore, RepoBlobstore, RepoBlobstoreArgs};
use repo_derived_data::{ArcRepoDerivedData, RepoDerivedData};
use repo_identity::{ArcRepoIdentity, RepoIdentity};
use requests_table::{ArcLongRunningRequestsQueue, SqlLongRunningRequestsQueue};
use scuba_ext::MononokeScubaSampleBuilder;
use segmented_changelog::{new_server_segmented_changelog, SegmentedChangelogSqlConnections};
use segmented_changelog_types::ArcSegmentedChangelog;
use slog::o;
use thiserror::Error;
use virtually_sharded_blobstore::VirtuallyShardedBlobstore;

pub use blobstore_factory::{BlobstoreOptions, ReadOnlyStorage};

#[derive(Clone)]
struct RepoFactoryCache<K: Clone + Eq + Hash, V: Clone> {
    cache: Arc<Mutex<HashMap<K, Arc<AsyncOnceCell<V>>>>>,
}

impl<K: Clone + Eq + Hash, V: Clone> RepoFactoryCache<K, V> {
    fn new() -> Self {
        RepoFactoryCache {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn get_or_try_init<F, Fut>(&self, key: &K, init: F) -> Result<V>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V>>,
    {
        let cell = {
            let mut cache = self.cache.lock();
            match cache.get(key) {
                Some(cell) => {
                    if let Some(value) = cell.get() {
                        return Ok(value.clone());
                    }
                    cell.clone()
                }
                None => {
                    let cell = Arc::new(AsyncOnceCell::new());
                    cache.insert(key.clone(), cell.clone());
                    cell
                }
            }
        };
        let value = cell.get_or_try_init(init).await?;
        Ok(value.clone())
    }
}

pub trait RepoFactoryOverride<T> = Fn(T) -> T + Send + Sync + 'static;

#[derive(Clone)]
pub struct RepoFactory {
    pub env: Arc<MononokeEnvironment>,
    censored_scuba_params: CensoredScubaParams,
    sql_factories: RepoFactoryCache<MetadataDatabaseConfig, Arc<MetadataSqlFactory>>,
    blobstores: RepoFactoryCache<BlobConfig, Arc<dyn Blobstore>>,
    redacted_blobs:
        RepoFactoryCache<MetadataDatabaseConfig, Arc<HashMap<String, RedactedMetadata>>>,
    blobstore_override: Option<Arc<dyn RepoFactoryOverride<Arc<dyn Blobstore>>>>,
    scrub_handler: Arc<dyn ScrubHandler>,
    blobstore_component_sampler: Option<Arc<dyn ComponentSamplingHandler>>,
}

impl RepoFactory {
    pub fn new(
        env: Arc<MononokeEnvironment>,
        censored_scuba_params: CensoredScubaParams,
    ) -> RepoFactory {
        RepoFactory {
            env,
            censored_scuba_params,
            sql_factories: RepoFactoryCache::new(),
            blobstores: RepoFactoryCache::new(),
            redacted_blobs: RepoFactoryCache::new(),
            blobstore_override: None,
            scrub_handler: default_scrub_handler(),
            blobstore_component_sampler: None,
        }
    }

    pub fn with_blobstore_override(
        &mut self,
        blobstore_override: impl RepoFactoryOverride<Arc<dyn Blobstore>>,
    ) -> &mut Self {
        self.blobstore_override = Some(Arc::new(blobstore_override));
        self
    }

    pub fn with_scrub_handler(&mut self, scrub_handler: Arc<dyn ScrubHandler>) -> &mut Self {
        self.scrub_handler = scrub_handler;
        self
    }

    pub fn with_blobstore_component_sampler(
        &mut self,
        handler: Arc<dyn ComponentSamplingHandler>,
    ) -> &mut Self {
        self.blobstore_component_sampler = Some(handler);
        self
    }

    pub async fn sql_factory(
        &self,
        config: &MetadataDatabaseConfig,
    ) -> Result<Arc<MetadataSqlFactory>> {
        self.sql_factories
            .get_or_try_init(config, || async move {
                let sql_factory = make_metadata_sql_factory(
                    self.env.fb,
                    config.clone(),
                    self.env.mysql_options.clone(),
                    self.env.readonly_storage,
                )
                .watched(&self.env.logger)
                .await?;
                Ok(Arc::new(sql_factory))
            })
            .await
    }

    async fn blobstore_no_cache(&self, config: &BlobConfig) -> Result<Arc<dyn Blobstore>> {
        make_blobstore(
            self.env.fb,
            config.clone(),
            &self.env.mysql_options,
            self.env.readonly_storage,
            &self.env.blobstore_options,
            &self.env.logger,
            &self.env.config_store,
            &self.scrub_handler,
            self.blobstore_component_sampler.as_ref(),
        )
        .watched(&self.env.logger)
        .await
    }

    async fn repo_blobstore_from_blobstore(
        &self,
        repo_identity: &ArcRepoIdentity,
        repo_config: &ArcRepoConfig,
        blobstore: &Arc<dyn Blobstore>,
    ) -> Result<Arc<RepoBlobstore>> {
        let mut blobstore = blobstore.clone();
        if self.env.readonly_storage.0 {
            blobstore = Arc::new(ReadOnlyBlobstore::new(blobstore));
        }

        let redacted_blobs = match repo_config.redaction {
            Redaction::Enabled => {
                let redacted_blobs = self
                    .redacted_blobs(&repo_config.storage_config.metadata)
                    .await?;
                // TODO: Make RepoBlobstore take Arc<...> so it can share the hashmap.
                Some(redacted_blobs.as_ref().clone())
            }
            Redaction::Disabled => None,
        };

        let censored_scuba_builder = self.censored_scuba_builder()?;

        let repo_blobstore_args = RepoBlobstoreArgs::new(
            blobstore,
            redacted_blobs,
            repo_identity.id(),
            censored_scuba_builder,
        );
        let (repo_blobstore, _repo_id) = repo_blobstore_args.into_blobrepo_parts();

        Ok(Arc::new(repo_blobstore))
    }

    async fn blobstore(&self, config: &BlobConfig) -> Result<Arc<dyn Blobstore>> {
        self.blobstores
            .get_or_try_init(config, || async move {
                let mut blobstore = self.blobstore_no_cache(config).await?;

                match self.env.caching {
                    Caching::Enabled(cache_shards) => {
                        let fb = self.env.fb;
                        let memcache_blobstore = tokio::task::spawn_blocking(move || {
                            new_memcache_blobstore(fb, blobstore, "multiplexed", "")
                        })
                        .await??;
                        blobstore = cachelib_blobstore(
                            memcache_blobstore,
                            cache_shards,
                            &self.env.blobstore_options.cachelib_options,
                        )?
                    }
                    Caching::CachelibOnlyBlobstore(cache_shards) => {
                        blobstore = cachelib_blobstore(
                            blobstore,
                            cache_shards,
                            &self.env.blobstore_options.cachelib_options,
                        )?;
                    }
                    Caching::Disabled => {}
                };

                if let Some(blobstore_override) = &self.blobstore_override {
                    blobstore = blobstore_override(blobstore);
                }

                Ok(blobstore)
            })
            .await
    }

    async fn redacted_blobs(
        &self,
        config: &MetadataDatabaseConfig,
    ) -> Result<Arc<HashMap<String, RedactedMetadata>>> {
        self.redacted_blobs
            .get_or_try_init(config, || async move {
                let sql_factory = self.sql_factory(config).await?;
                let redacted_content_store = sql_factory.open::<SqlRedactedContentStore>()?;
                // Fetch redacted blobs in a separate task so that slow polls
                // in repo construction don't interfere with the SQL query.
                let redacted_blobs = tokio::task::spawn(async move {
                    redacted_content_store.get_all_redacted_blobs().await
                })
                .await??;
                Ok(Arc::new(redacted_blobs))
            })
            .await
    }

    /// Returns a named volatile pool if caching is enabled.
    fn maybe_volatile_pool(&self, name: &str) -> Result<Option<cachelib::VolatileLruCachePool>> {
        match self.env.caching {
            Caching::Enabled(_) => Ok(Some(volatile_pool(name)?)),
            _ => Ok(None),
        }
    }

    fn censored_scuba_builder(&self) -> Result<MononokeScubaSampleBuilder> {
        let mut builder = MononokeScubaSampleBuilder::with_opt_table(
            self.env.fb,
            self.censored_scuba_params.table.clone(),
        );
        builder.add_common_server_data();
        if let Some(scuba_log_file) = &self.censored_scuba_params.local_path {
            builder = builder.with_log_file(scuba_log_file)?;
        }
        Ok(builder)
    }
}

fn cache_pool(name: &str) -> Result<cachelib::LruCachePool> {
    Ok(cachelib::get_pool(name)
        .ok_or_else(|| RepoFactoryError::MissingCachePool(name.to_string()))?)
}

fn volatile_pool(name: &str) -> Result<cachelib::VolatileLruCachePool> {
    Ok(cachelib::get_volatile_pool(name)?
        .ok_or_else(|| RepoFactoryError::MissingCachePool(name.to_string()))?)
}

pub fn cachelib_blobstore<B: Blobstore + 'static>(
    blobstore: B,
    cache_shards: usize,
    options: &CachelibBlobstoreOptions,
) -> Result<Arc<dyn Blobstore>> {
    const BLOBSTORE_BLOBS_CACHE_POOL: &str = "blobstore-blobs";
    const BLOBSTORE_PRESENCE_CACHE_POOL: &str = "blobstore-presence";

    let blobstore: Arc<dyn Blobstore> = match NonZeroUsize::new(cache_shards) {
        Some(cache_shards) => {
            let blob_pool = volatile_pool(BLOBSTORE_BLOBS_CACHE_POOL)?;
            let presence_pool = volatile_pool(BLOBSTORE_PRESENCE_CACHE_POOL)?;

            Arc::new(VirtuallyShardedBlobstore::new(
                blobstore,
                blob_pool,
                presence_pool,
                cache_shards,
                options.clone(),
            ))
        }
        None => {
            let blob_pool = cache_pool(BLOBSTORE_BLOBS_CACHE_POOL)?;
            let presence_pool = cache_pool(BLOBSTORE_PRESENCE_CACHE_POOL)?;

            Arc::new(new_cachelib_blobstore_no_lease(
                blobstore,
                Arc::new(blob_pool),
                Arc::new(presence_pool),
                options.clone(),
            ))
        }
    };

    Ok(blobstore)
}

#[derive(Debug, Error)]
pub enum RepoFactoryError {
    #[error("Error opening changesets")]
    Changesets,

    #[error("Error opening bookmarks")]
    Bookmarks,

    #[error("Error opening phases")]
    Phases,

    #[error("Error opening bonsai-hg mapping")]
    BonsaiHgMapping,

    #[error("Error opening bonsai-git mapping")]
    BonsaiGitMapping,

    #[error("Error opening bonsai-globalrev mapping")]
    BonsaiGlobalrevMapping,

    #[error("Error opening bonsai-svnrev mapping")]
    BonsaiSvnrevMapping,

    #[error("Error opening pushrebase mutation mapping")]
    PushrebaseMutationMapping,

    #[error("Error opening filenodes")]
    Filenodes,

    #[error("Error opening hg mutation store")]
    HgMutationStore,

    #[error("Error opening segmented changelog")]
    SegmentedChangelog,

    #[error("Missing cache pool: {0}")]
    MissingCachePool(String),

    #[error("Error opening long-running request queue")]
    LongRunningRequestsQueue,
}

#[facet::factory(name: String, config: RepoConfig)]
impl RepoFactory {
    pub fn repo_config(&self, config: &RepoConfig) -> ArcRepoConfig {
        Arc::new(config.clone())
    }

    pub fn repo_identity(&self, name: &str, repo_config: &ArcRepoConfig) -> ArcRepoIdentity {
        Arc::new(RepoIdentity::new(repo_config.repoid, name.to_string()))
    }

    pub fn caching(&self) -> Caching {
        self.env.caching
    }

    pub async fn changesets(
        &self,
        repo_identity: &ArcRepoIdentity,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcChangesets> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let builder = sql_factory
            .open::<SqlChangesetsBuilder>()
            .context(RepoFactoryError::Changesets)?;
        let changesets = builder.build(self.env.rendezvous_options, repo_identity.id());
        if let Some(pool) = self.maybe_volatile_pool("changesets")? {
            Ok(Arc::new(CachingChangesets::new(
                self.env.fb,
                Arc::new(changesets),
                pool,
            )))
        } else {
            Ok(Arc::new(changesets))
        }
    }

    pub fn changeset_fetcher(
        &self,
        repo_identity: &ArcRepoIdentity,
        changesets: &ArcChangesets,
    ) -> ArcChangesetFetcher {
        Arc::new(SimpleChangesetFetcher::new(
            changesets.clone(),
            repo_identity.id(),
        ))
    }

    pub async fn sql_bookmarks(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
    ) -> Result<ArcSqlBookmarks> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let sql_bookmarks = sql_factory
            .open::<SqlBookmarksBuilder>()
            .context(RepoFactoryError::Bookmarks)?
            .with_repo_id(repo_identity.id());

        Ok(Arc::new(sql_bookmarks))
    }

    pub fn bookmarks(
        &self,
        sql_bookmarks: &ArcSqlBookmarks,
        repo_identity: &ArcRepoIdentity,
    ) -> ArcBookmarks {
        Arc::new(CachedBookmarks::new(
            sql_bookmarks.clone(),
            repo_identity.id(),
        ))
    }

    pub fn bookmark_update_log(&self, sql_bookmarks: &ArcSqlBookmarks) -> ArcBookmarkUpdateLog {
        sql_bookmarks.clone()
    }

    pub async fn sql_phases_factory(
        &self,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcSqlPhasesFactory> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let mut sql_phases_factory = sql_factory
            .open::<SqlPhasesFactory>()
            .context(RepoFactoryError::Phases)?;
        if let Some(pool) = self.maybe_volatile_pool("phases")? {
            sql_phases_factory.enable_caching(self.env.fb, pool);
        }
        Ok(Arc::new(sql_phases_factory))
    }

    pub async fn bonsai_hg_mapping(
        &self,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcBonsaiHgMapping> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let builder = sql_factory
            .open::<SqlBonsaiHgMappingBuilder>()
            .context(RepoFactoryError::BonsaiHgMapping)?;
        let bonsai_hg_mapping = builder.build(self.env.rendezvous_options);
        if let Some(pool) = self.maybe_volatile_pool("bonsai_hg_mapping")? {
            Ok(Arc::new(CachingBonsaiHgMapping::new(
                self.env.fb,
                Arc::new(bonsai_hg_mapping),
                pool,
            )))
        } else {
            Ok(Arc::new(bonsai_hg_mapping))
        }
    }

    pub async fn bonsai_git_mapping(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
    ) -> Result<ArcBonsaiGitMapping> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let bonsai_git_mapping = sql_factory
            .open::<SqlBonsaiGitMappingConnection>()
            .context(RepoFactoryError::BonsaiGitMapping)?
            .with_repo_id(repo_identity.id());
        Ok(Arc::new(bonsai_git_mapping))
    }

    pub async fn long_running_requests_queue(
        &self,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcLongRunningRequestsQueue> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let long_running_requests_queue = sql_factory
            .open::<SqlLongRunningRequestsQueue>()
            .context(RepoFactoryError::LongRunningRequestsQueue)?;
        Ok(Arc::new(long_running_requests_queue))
    }

    pub async fn bonsai_globalrev_mapping(
        &self,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcBonsaiGlobalrevMapping> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let bonsai_globalrev_mapping = sql_factory
            .open::<SqlBonsaiGlobalrevMapping>()
            .context(RepoFactoryError::BonsaiGlobalrevMapping)?;
        if let Some(pool) = self.maybe_volatile_pool("bonsai_globalrev_mapping")? {
            Ok(Arc::new(CachingBonsaiGlobalrevMapping::new(
                self.env.fb,
                Arc::new(bonsai_globalrev_mapping),
                pool,
            )))
        } else {
            Ok(Arc::new(bonsai_globalrev_mapping))
        }
    }

    pub async fn pushrebase_mutation_mapping(
        &self,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcPushrebaseMutationMapping> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let conn = sql_factory
            .open::<SqlPushrebaseMutationMappingConnection>()
            .context(RepoFactoryError::PushrebaseMutationMapping)?;
        Ok(Arc::new(conn.with_repo_id(repo_config.repoid)))
    }

    pub async fn repo_bonsai_svnrev_mapping(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
    ) -> Result<ArcRepoBonsaiSvnrevMapping> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let bonsai_svnrev_mapping = sql_factory
            .open::<SqlBonsaiSvnrevMapping>()
            .context(RepoFactoryError::BonsaiSvnrevMapping)?;
        let bonsai_svnrev_mapping: Arc<dyn BonsaiSvnrevMapping + Send + Sync> =
            if let Some(pool) = self.maybe_volatile_pool("bonsai_svnrev_mapping")? {
                Arc::new(CachingBonsaiSvnrevMapping::new(
                    self.env.fb,
                    Arc::new(bonsai_svnrev_mapping),
                    pool,
                ))
            } else {
                Arc::new(bonsai_svnrev_mapping)
            };
        Ok(Arc::new(RepoBonsaiSvnrevMapping::new(
            repo_identity.id(),
            bonsai_svnrev_mapping,
        )))
    }

    pub async fn filenodes(&self, repo_config: &ArcRepoConfig) -> Result<ArcFilenodes> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let mut filenodes_builder = sql_factory
            .open_shardable::<NewFilenodesBuilder>()
            .context(RepoFactoryError::Filenodes)?;
        if let Caching::Enabled(_) = self.env.caching {
            let filenodes_tier = sql_factory.tier_info_shardable::<NewFilenodesBuilder>()?;
            let filenodes_pool = self
                .maybe_volatile_pool("filenodes")?
                .ok_or(RepoFactoryError::Filenodes)?;
            let filenodes_history_pool = self
                .maybe_volatile_pool("filenodes_history")?
                .ok_or(RepoFactoryError::Filenodes)?;
            filenodes_builder.enable_caching(
                self.env.fb,
                filenodes_pool,
                filenodes_history_pool,
                "newfilenodes",
                &filenodes_tier.tier_name,
            );
        }
        Ok(Arc::new(filenodes_builder.build()))
    }

    pub async fn hg_mutation_store(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
    ) -> Result<ArcHgMutationStore> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let hg_mutation_store = sql_factory
            .open::<SqlHgMutationStoreBuilder>()
            .context(RepoFactoryError::HgMutationStore)?
            .with_repo_id(repo_identity.id());
        Ok(Arc::new(hg_mutation_store))
    }

    pub async fn segmented_changelog(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
        changeset_fetcher: &ArcChangesetFetcher,
        bookmarks: &ArcBookmarks,
        repo_blobstore: &ArcRepoBlobstore,
    ) -> Result<ArcSegmentedChangelog> {
        let sql_factory = self
            .sql_factory(&repo_config.storage_config.metadata)
            .await?;
        let sql_connections = sql_factory
            .open::<SegmentedChangelogSqlConnections>()
            .context(RepoFactoryError::SegmentedChangelog)?;
        let pool = self.maybe_volatile_pool("segmented_changelog")?;
        let repo_name = String::from(repo_identity.name());
        let logger = self.env.logger.new(o!("repo" => repo_name));
        let session = SessionContainer::new_with_defaults(self.env.fb);
        let ctx = session.new_context(logger, self.env.scuba_sample_builder.clone());
        let segmented_changelog = new_server_segmented_changelog(
            self.env.fb,
            &ctx,
            repo_identity.id(),
            repo_config.segmented_changelog_config.clone(),
            sql_connections,
            changeset_fetcher.clone(),
            bookmarks.clone(),
            repo_blobstore.clone(),
            pool,
        )
        .await
        .context(RepoFactoryError::SegmentedChangelog)?;
        Ok(Arc::new(segmented_changelog))
    }

    pub fn repo_derived_data(&self, repo_config: &ArcRepoConfig) -> Result<ArcRepoDerivedData> {
        let config = repo_config.derived_data_config.clone();
        // Derived data leasing is performed through the cache, so is only
        // available if caching is enabled.
        let lease: Arc<dyn LeaseOps> = if let Caching::Enabled(_) = self.env.caching {
            Arc::new(MemcacheOps::new(self.env.fb, "derived-data-lease", "")?)
        } else {
            Arc::new(InProcessLease::new())
        };
        Ok(Arc::new(RepoDerivedData::new(config, lease)))
    }

    pub async fn skiplist_index(
        &self,
        repo_config: &ArcRepoConfig,
        repo_identity: &ArcRepoIdentity,
        repo_blobstore: &ArcRepoBlobstore,
    ) -> Result<ArcSkiplistIndex> {
        let repo_name = String::from(repo_identity.name());
        let logger = self.env.logger.new(o!("repo" => repo_name));
        let session = SessionContainer::new_with_defaults(self.env.fb);
        let ctx = session.new_context(logger, self.env.scuba_sample_builder.clone());
        SkiplistIndex::from_blobstore(
            &ctx,
            &repo_config.skiplist_index_blobstore_key,
            &repo_blobstore.boxed(),
        )
        .await
    }

    pub async fn repo_blobstore(
        &self,
        repo_identity: &ArcRepoIdentity,
        repo_config: &ArcRepoConfig,
    ) -> Result<ArcRepoBlobstore> {
        let blobstore = self
            .blobstore(&repo_config.storage_config.blobstore)
            .await?;
        self.repo_blobstore_from_blobstore(repo_identity, repo_config, &blobstore)
            .await
    }

    pub fn filestore_config(&self, repo_config: &ArcRepoConfig) -> ArcFilestoreConfig {
        let filestore_config = repo_config
            .filestore
            .as_ref()
            .map(|p| FilestoreConfig {
                chunk_size: Some(p.chunk_size),
                concurrency: p.concurrency,
            })
            .unwrap_or_default();
        Arc::new(filestore_config)
    }
}
