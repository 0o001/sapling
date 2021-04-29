/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use super::{ChangesetEntry, ChangesetInsert, Changesets, SortOrder};
use anyhow::Error;
use async_trait::async_trait;
use bytes::Bytes;
use caching_ext::{
    get_or_fill, CacheDisposition, CacheTtl, CachelibHandler, EntityStore, KeyedEntityStore,
    MemcacheEntity, MemcacheHandler,
};
use changeset_entry_thrift as thrift;
use context::CoreContext;
use fbinit::FacebookInit;
use fbthrift::compact_protocol;
use futures::stream::BoxStream;
use maplit::hashset;
use memcache::{KeyGen, MemcacheClient};
use mononoke_types::{
    ChangesetId, ChangesetIdPrefix, ChangesetIdsResolvedFromPrefix, RepositoryId,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[cfg(test)]
use caching_ext::MockStoreStats;

pub fn get_cache_key(repo_id: RepositoryId, cs_id: &ChangesetId) -> String {
    format!("{}.{}", repo_id.prefix(), cs_id).to_string()
}

#[derive(Clone)]
pub struct CachingChangesets {
    changesets: Arc<dyn Changesets>,
    cachelib: CachelibHandler<ChangesetEntry>,
    memcache: MemcacheHandler,
    keygen: KeyGen,
}

fn get_keygen() -> KeyGen {
    let key_prefix = "scm.mononoke.changesets";

    KeyGen::new(
        key_prefix,
        thrift::MC_CODEVER as u32,
        thrift::MC_SITEVER as u32,
    )
}

impl CachingChangesets {
    pub fn new(
        fb: FacebookInit,
        changesets: Arc<dyn Changesets>,
        cache_pool: cachelib::VolatileLruCachePool,
    ) -> Self {
        Self {
            changesets,
            cachelib: cache_pool.into(),
            memcache: MemcacheClient::new(fb)
                .expect("Memcache initialization failed")
                .into(),
            keygen: get_keygen(),
        }
    }

    #[cfg(test)]
    pub fn mocked(changesets: Arc<dyn Changesets>) -> Self {
        let cachelib = CachelibHandler::create_mock();
        let memcache = MemcacheHandler::create_mock();

        Self {
            changesets,
            cachelib,
            memcache,
            keygen: get_keygen(),
        }
    }

    #[cfg(test)]
    pub fn fork_cachelib(&self) -> Self {
        Self {
            changesets: self.changesets.clone(),
            cachelib: CachelibHandler::create_mock(),
            memcache: self.memcache.clone(),
            keygen: self.keygen.clone(),
        }
    }

    #[cfg(test)]
    pub fn cachelib_stats(&self) -> MockStoreStats {
        match self.cachelib {
            CachelibHandler::Real(_) => unimplemented!(),
            CachelibHandler::Mock(ref mock) => mock.stats(),
        }
    }

    #[cfg(test)]
    pub fn memcache_stats(&self) -> MockStoreStats {
        match self.memcache {
            MemcacheHandler::Real(_) => unimplemented!(),
            MemcacheHandler::Mock(ref mock) => mock.stats(),
        }
    }
}

#[async_trait]
impl Changesets for CachingChangesets {
    async fn add(&self, ctx: CoreContext, cs: ChangesetInsert) -> Result<bool, Error> {
        self.changesets.add(ctx, cs).await
    }

    async fn get(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_id: ChangesetId,
    ) -> Result<Option<ChangesetEntry>, Error> {
        let ctx = (&ctx, repo_id, self);
        let mut map = get_or_fill(ctx, hashset![cs_id]).await?;
        Ok(map.remove(&cs_id))
    }

    async fn get_many(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_ids: Vec<ChangesetId>,
    ) -> Result<Vec<ChangesetEntry>, Error> {
        let ctx = (&ctx, repo_id, self);
        let res = get_or_fill(ctx, cs_ids.into_iter().collect())
            .await?
            .into_iter()
            .map(|(_, val)| val)
            .collect();
        Ok(res)
    }

    /// Use caching for the full changeset ids and slower path otherwise.
    async fn get_many_by_prefix(
        &self,
        ctx: CoreContext,
        repo_id: RepositoryId,
        cs_prefix: ChangesetIdPrefix,
        limit: usize,
    ) -> Result<ChangesetIdsResolvedFromPrefix, Error> {
        if let Some(id) = cs_prefix.into_changeset_id() {
            let res = self.get(ctx, repo_id, id).await?;
            return match res {
                Some(_) if limit > 0 => Ok(ChangesetIdsResolvedFromPrefix::Single(id)),
                _ => Ok(ChangesetIdsResolvedFromPrefix::NoMatch),
            };
        }
        self.changesets
            .get_many_by_prefix(ctx, repo_id, cs_prefix, limit)
            .await
    }

    fn prime_cache(&self, _ctx: &CoreContext, changesets: &[ChangesetEntry]) {
        for cs in changesets {
            let key = get_cache_key(cs.repo_id, &cs.cs_id);
            let _ = self.cachelib.set_cached(&key, &cs);
        }
    }

    async fn enumeration_bounds(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        read_from_master: bool,
    ) -> Result<Option<(u64, u64)>, Error> {
        self.changesets
            .enumeration_bounds(ctx, repo_id, read_from_master)
            .await
    }

    fn list_enumeration_range(
        &self,
        ctx: &CoreContext,
        repo_id: RepositoryId,
        min_id: u64,
        max_id: u64,
        sort_and_limit: Option<(SortOrder, u64)>,
        read_from_master: bool,
    ) -> BoxStream<'_, Result<(ChangesetId, u64), Error>> {
        self.changesets.list_enumeration_range(
            ctx,
            repo_id,
            min_id,
            max_id,
            sort_and_limit,
            read_from_master,
        )
    }
}

impl MemcacheEntity for ChangesetEntry {
    fn serialize(&self) -> Bytes {
        compact_protocol::serialize(&self.clone().into_thrift())
    }

    fn deserialize(bytes: Bytes) -> Result<Self, ()> {
        compact_protocol::deserialize(bytes)
            .and_then(ChangesetEntry::from_thrift)
            .map_err(|_| ())
    }
}

type CacheRequest<'a> = (&'a CoreContext, RepositoryId, &'a CachingChangesets);

impl EntityStore<ChangesetEntry> for CacheRequest<'_> {
    fn cachelib(&self) -> &CachelibHandler<ChangesetEntry> {
        let (_, _, mapping) = self;
        &mapping.cachelib
    }

    fn keygen(&self) -> &KeyGen {
        let (_, _, mapping) = self;
        &mapping.keygen
    }

    fn memcache(&self) -> &MemcacheHandler {
        let (_, _, mapping) = self;
        &mapping.memcache
    }

    fn cache_determinator(&self, _: &ChangesetEntry) -> CacheDisposition {
        CacheDisposition::Cache(CacheTtl::NoTtl)
    }

    caching_ext::impl_singleton_stats!("changesets");

    #[cfg(test)]
    fn spawn_memcache_writes(&self) -> bool {
        let (_, _, mapping) = self;

        match mapping.memcache {
            MemcacheHandler::Real(_) => true,
            MemcacheHandler::Mock(..) => false,
        }
    }
}

#[async_trait]
impl KeyedEntityStore<ChangesetId, ChangesetEntry> for CacheRequest<'_> {
    fn get_cache_key(&self, cs_id: &ChangesetId) -> String {
        let (_, repo_id, _) = self;
        get_cache_key(*repo_id, cs_id)
    }

    async fn get_from_db(
        &self,
        keys: HashSet<ChangesetId>,
    ) -> Result<HashMap<ChangesetId, ChangesetEntry>, Error> {
        let (ctx, repo_id, mapping) = self;

        let res = mapping
            .changesets
            .get_many((*ctx).clone(), *repo_id, keys.into_iter().collect())
            .await?;

        Result::<_, Error>::Ok(res.into_iter().map(|e| (e.cs_id, e)).collect())
    }
}
