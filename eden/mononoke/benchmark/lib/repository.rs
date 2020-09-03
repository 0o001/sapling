/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! Main function is `new_benchmark_repo` which creates `BlobRepo` which delay applied
//! to all underlying stores, but which all the caching enabled.
use anyhow::{Error, Result};
use blobrepo::BlobRepo;
use blobrepo_factory::init_all_derived_data;
use blobstore::Blobstore;
use bonsai_git_mapping::SqlBonsaiGitMappingConnection;
use bonsai_globalrev_mapping::SqlBonsaiGlobalrevMapping;
use bonsai_hg_mapping::{
    BonsaiHgMapping, BonsaiHgMappingEntry, BonsaiOrHgChangesetIds, CachingBonsaiHgMapping,
    SqlBonsaiHgMapping,
};
use cacheblob::{dummy::DummyLease, new_cachelib_blobstore, CachelibBlobstoreOptions};
use changeset_fetcher::SimpleChangesetFetcher;
use changesets::{CachingChangesets, ChangesetEntry, ChangesetInsert, Changesets, SqlChangesets};
use context::CoreContext;
use dbbookmarks::SqlBookmarksBuilder;
use delayblob::DelayedBlobstore;
use fbinit::FacebookInit;
use filenodes::{FilenodeInfo, FilenodeRangeResult, FilenodeResult, Filenodes, PreparedFilenode};
use filestore::FilestoreConfig;
use futures_ext::{BoxFuture, FutureExt};
use futures_old::{future, Future};
use memblob::EagerMemblob;
use mercurial_mutation::SqlHgMutationStoreBuilder;
use mercurial_types::{HgChangesetIdPrefix, HgChangesetIdsResolvedFromPrefix, HgFileNodeId};
use mononoke_types::{
    ChangesetId, ChangesetIdPrefix, ChangesetIdsResolvedFromPrefix, RepoPath, RepositoryId,
};
use newfilenodes::NewFilenodesBuilder;
use phases::SqlPhasesFactory;
use rand::Rng;
use rand_distr::Distribution;
use repo_blobstore::RepoBlobstoreArgs;
use scuba_ext::ScubaSampleBuilder;
use segmented_changelog::DisabledSegmentedChangelog;
use sql_construct::SqlConstruct;
use std::{sync::Arc, time::Duration};

pub type Normal = rand_distr::Normal<f64>;

pub struct DelaySettings {
    pub blobstore_put_dist: Normal,
    pub blobstore_get_dist: Normal,
    pub db_put_dist: Normal,
    pub db_get_dist: Normal,
}

impl Default for DelaySettings {
    fn default() -> Self {
        Self {
            blobstore_put_dist: Normal::new(0.1, 0.05).expect("Normal::new failed"),
            blobstore_get_dist: Normal::new(0.05, 0.025).expect("Normal::new failed"),
            db_put_dist: Normal::new(0.02, 0.01).expect("Normal::new failed"),
            db_get_dist: Normal::new(0.02, 0.01).expect("Normal::new failed"),
        }
    }
}

pub fn new_benchmark_repo(fb: FacebookInit, settings: DelaySettings) -> Result<BlobRepo> {
    let blobstore: Arc<dyn Blobstore> = {
        let delayed: Arc<dyn Blobstore> = Arc::new(DelayedBlobstore::new(
            EagerMemblob::new(),
            settings.blobstore_get_dist,
            settings.blobstore_put_dist,
        ));
        Arc::new(new_cachelib_blobstore(
            delayed,
            Arc::new(cachelib::get_pool("blobstore-blobs").ok_or(Error::msg("no cache pool"))?),
            Arc::new(cachelib::get_pool("blobstore-presence").ok_or(Error::msg("no cache pool"))?),
            CachelibBlobstoreOptions::default(),
        ))
    };

    let filenodes = {
        let pool = cachelib::get_volatile_pool("filenodes")
            .unwrap()
            .ok_or(Error::msg("no cache pool"))?;

        let mut builder = NewFilenodesBuilder::with_sqlite_in_memory()?;
        builder.enable_caching(fb, pool.clone(), pool, "filenodes", "");

        let filenodes: Arc<dyn Filenodes> = Arc::new(DelayedFilenodes::new(
            builder.build(),
            settings.db_get_dist,
            settings.db_put_dist,
        ));

        filenodes
    };

    let changesets = {
        let changesets: Arc<dyn Changesets> = Arc::new(DelayedChangesets::new(
            SqlChangesets::with_sqlite_in_memory()?,
            settings.db_get_dist,
            settings.db_put_dist,
        ));
        Arc::new(CachingChangesets::new(
            fb,
            changesets,
            cachelib::get_volatile_pool("changesets")
                .unwrap()
                .ok_or(Error::msg("no cache pool"))?,
        ))
    };

    let bonsai_globalrev_mapping = Arc::new(SqlBonsaiGlobalrevMapping::with_sqlite_in_memory()?);

    let bonsai_hg_mapping = {
        let mapping: Arc<dyn BonsaiHgMapping> = Arc::new(DelayedBonsaiHgMapping::new(
            SqlBonsaiHgMapping::with_sqlite_in_memory()?,
            settings.db_get_dist,
            settings.db_put_dist,
        ));
        Arc::new(CachingBonsaiHgMapping::new(
            fb,
            mapping,
            cachelib::get_volatile_pool("bonsai_hg_mapping")
                .unwrap()
                .ok_or(Error::msg("no cache pool"))?,
        ))
    };

    // Disable redaction check when executing benchmark reports
    let repoid = RepositoryId::new(rand::random());

    // TODO:
    //  - add caching
    //  - add delay
    let bookmarks = Arc::new(SqlBookmarksBuilder::with_sqlite_in_memory()?.with_repo_id(repoid));

    let bonsai_git_mapping =
        Arc::new(SqlBonsaiGitMappingConnection::with_sqlite_in_memory()?.with_repo_id(repoid));

    let phases_factory = SqlPhasesFactory::with_sqlite_in_memory()?;

    let hg_mutation_store =
        Arc::new(SqlHgMutationStoreBuilder::with_sqlite_in_memory()?.with_repo_id(repoid));

    let changeset_fetcher = Arc::new(SimpleChangesetFetcher::new(changesets.clone(), repoid));

    let blobstore =
        RepoBlobstoreArgs::new(blobstore, None, repoid, ScubaSampleBuilder::with_discard());
    Ok(blobrepo_factory::blobrepo_new(
        bookmarks.clone(),
        bookmarks,
        blobstore,
        filenodes,
        changesets,
        changeset_fetcher,
        bonsai_git_mapping,
        bonsai_globalrev_mapping,
        bonsai_hg_mapping,
        hg_mutation_store,
        Arc::new(DummyLease {}),
        Arc::new(DisabledSegmentedChangelog::new()),
        FilestoreConfig::default(),
        phases_factory,
        init_all_derived_data(),
        "benchmarkrepo".to_string(),
    ))
}

/// Delay target future execution by delay sampled from provided distribution
fn delay<F, D>(distribution: D, target: F) -> impl Future<Item = F::Item, Error = Error>
where
    D: Distribution<f64>,
    F: Future<Error = Error>,
{
    future::lazy(move || {
        let seconds = rand::thread_rng().sample(distribution).abs();
        tokio_timer::sleep(Duration::new(
            seconds.trunc() as u64,
            (seconds.fract() * 1e+9) as u32,
        ))
        .from_err()
        .and_then(move |_| target)
    })
}

struct DelayedFilenodes<F> {
    inner: F,
    get_dist: Normal,
    put_dist: Normal,
}

impl<F> DelayedFilenodes<F> {
    fn new(inner: F, get_dist: Normal, put_dist: Normal) -> Self {
        Self {
            inner,
            get_dist,
            put_dist,
        }
    }
}

impl<F: Filenodes> Filenodes for DelayedFilenodes<F> {
    fn add_filenodes(
        &self,
        ctx: CoreContext,
        info: Vec<PreparedFilenode>,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<()>, Error> {
        delay(self.put_dist, self.inner.add_filenodes(ctx, info, repo_id)).boxify()
    }

    fn add_or_replace_filenodes(
        &self,
        ctx: CoreContext,
        info: Vec<PreparedFilenode>,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<()>, Error> {
        delay(
            self.put_dist,
            self.inner.add_or_replace_filenodes(ctx, info, repo_id),
        )
        .boxify()
    }

    fn get_filenode(
        &self,
        ctx: CoreContext,
        path: &RepoPath,
        filenode: HgFileNodeId,
        repo_id: RepositoryId,
    ) -> BoxFuture<FilenodeResult<Option<FilenodeInfo>>, Error> {
        delay(
            self.get_dist,
            self.inner.get_filenode(ctx, path, filenode, repo_id),
        )
        .boxify()
    }

    fn get_all_filenodes_maybe_stale(
        &self,
        ctx: CoreContext,
        path: &RepoPath,
        repo_id: RepositoryId,
        limit: Option<u64>,
    ) -> BoxFuture<FilenodeRangeResult<Vec<FilenodeInfo>>, Error> {
        delay(
            self.get_dist,
            self.inner
                .get_all_filenodes_maybe_stale(ctx, path, repo_id, limit),
        )
        .boxify()
    }

    fn prime_cache(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        filenodes: &[PreparedFilenode],
    ) {
        self.inner.prime_cache(ctx, repo_id, filenodes)
    }
}

struct DelayedChangesets<C> {
    inner: C,
    get_dist: Normal,
    put_dist: Normal,
}

impl<C> DelayedChangesets<C> {
    fn new(inner: C, get_dist: Normal, put_dist: Normal) -> Self {
        Self {
            inner,
            get_dist,
            put_dist,
        }
    }
}

impl<C: Changesets> Changesets for DelayedChangesets<C> {
    fn add(&self, ctx: CoreContext, cs: ChangesetInsert) -> BoxFuture<bool, Error> {
        delay(self.put_dist, self.inner.add(ctx, cs)).boxify()
    }

    fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> BoxFuture<Option<ChangesetEntry>, Error> {
        delay(self.get_dist, self.inner.get(ctx, repo_id, cs_id)).boxify()
    }

    fn get_many(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_ids: Vec<ChangesetId>,
    ) -> BoxFuture<Vec<ChangesetEntry>, Error> {
        delay(self.get_dist, self.inner.get_many(ctx, repo_id, cs_ids)).boxify()
    }

    fn get_many_by_prefix(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_prefix: ChangesetIdPrefix,
        limit: usize,
    ) -> BoxFuture<ChangesetIdsResolvedFromPrefix, Error> {
        delay(
            self.get_dist,
            self.inner
                .get_many_by_prefix(ctx, repo_id, cs_prefix, limit),
        )
        .boxify()
    }

    fn prime_cache(&self, ctx: &CoreContext, changesets: &[ChangesetEntry]) {
        self.inner.prime_cache(ctx, changesets)
    }

    fn get_sql_changesets(&self) -> &SqlChangesets {
        self.inner.get_sql_changesets()
    }
}

struct DelayedBonsaiHgMapping<M> {
    inner: M,
    get_dist: Normal,
    put_dist: Normal,
}

impl<M> DelayedBonsaiHgMapping<M> {
    fn new(inner: M, get_dist: Normal, put_dist: Normal) -> Self {
        Self {
            inner,
            get_dist,
            put_dist,
        }
    }
}

impl<M: BonsaiHgMapping> BonsaiHgMapping for DelayedBonsaiHgMapping<M> {
    fn add(&self, ctx: CoreContext, entry: BonsaiHgMappingEntry) -> BoxFuture<bool, Error> {
        delay(self.put_dist, self.inner.add(ctx, entry)).boxify()
    }

    fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_id: BonsaiOrHgChangesetIds,
    ) -> BoxFuture<Vec<BonsaiHgMappingEntry>, Error> {
        delay(self.get_dist, self.inner.get(ctx, repo_id, cs_id)).boxify()
    }

    fn get_many_hg_by_prefix(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_prefix: HgChangesetIdPrefix,
        limit: usize,
    ) -> BoxFuture<HgChangesetIdsResolvedFromPrefix, Error> {
        delay(
            self.get_dist,
            self.inner
                .get_many_hg_by_prefix(ctx, repo_id, cs_prefix, limit),
        )
        .boxify()
    }
}
