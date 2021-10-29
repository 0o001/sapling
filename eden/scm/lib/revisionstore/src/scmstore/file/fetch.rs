/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::Result;
use async_runtime::block_on;
use async_runtime::spawn_blocking;
use async_runtime::stream_to_iter;
use edenapi_types::FileEntry;
use edenapi_types::FileSpec;
use futures::StreamExt;
use parking_lot::RwLock;
use tracing::field;
use tracing::instrument;
use types::Key;
use types::Sha256;

use crate::datastore::HgIdDataStore;
use crate::datastore::RemoteDataStore;
use crate::fetch_logger::FetchLogger;
use crate::indexedlogauxstore::AuxStore;
use crate::indexedlogauxstore::Entry as AuxDataEntry;
use crate::indexedlogdatastore::Entry;
use crate::indexedlogdatastore::IndexedLogHgIdDataStore;
use crate::indexedlogutil::StoreType;
use crate::lfs::LfsPointersEntry;
use crate::lfs::LfsRemoteInner;
use crate::lfs::LfsStore;
use crate::lfs::LfsStoreEntry;
use crate::memcache::McData;
use crate::scmstore::attrs::StoreAttrs;
use crate::scmstore::fetch::CommonFetchState;
use crate::scmstore::fetch::FetchErrors;
use crate::scmstore::fetch::FetchResults;
use crate::scmstore::file::metrics::FileStoreFetchMetrics;
use crate::scmstore::file::LazyFile;
use crate::scmstore::value::StoreValue;
use crate::scmstore::FileAttributes;
use crate::scmstore::FileAuxData;
use crate::scmstore::FileStore;
use crate::scmstore::StoreFile;
use crate::util;
use crate::ContentHash;
use crate::ContentStore;
use crate::EdenApiFileStore;
use crate::ExtStoredPolicy;
use crate::MemcacheStore;
use crate::Metadata;
use crate::StoreKey;

pub struct FetchState {
    common: CommonFetchState<StoreFile>,

    /// Errors encountered during fetching.
    errors: FetchErrors,

    /// LFS pointers we've discovered corresponding to a request Key.
    lfs_pointers: HashMap<Key, (LfsPointersEntry, bool)>,

    /// A table tracking if discovered LFS pointers were found in the local-only or cache / shared store.
    pointer_origin: Arc<RwLock<HashMap<Sha256, StoreType>>>,

    /// A table tracking if each key is local-only or cache/shared so that computed aux data can be written to the appropriate store
    key_origin: HashMap<Key, StoreType>,

    /// Attributes computed from other attributes, may be cached locally (currently only aux_data may be computed)
    computed_aux_data: HashMap<Key, StoreType>,

    /// Tracks remote fetches which match a specific regex
    fetch_logger: Option<Arc<FetchLogger>>,

    /// Track fetch metrics,
    metrics: FileStoreFetchMetrics,

    // Config
    extstored_policy: ExtStoredPolicy,
    compute_aux_data: bool,
}

impl FetchState {
    pub(crate) fn new(
        keys: impl Iterator<Item = Key>,
        attrs: FileAttributes,
        file_store: &FileStore,
    ) -> Self {
        FetchState {
            common: CommonFetchState::new(keys, attrs),
            errors: FetchErrors::new(),
            metrics: FileStoreFetchMetrics::default(),

            lfs_pointers: HashMap::new(),
            key_origin: HashMap::new(),
            pointer_origin: Arc::new(RwLock::new(HashMap::new())),

            computed_aux_data: HashMap::new(),

            fetch_logger: file_store.fetch_logger.clone(),
            extstored_policy: file_store.extstored_policy,
            compute_aux_data: true,
        }
    }

    /// Return all incomplete requested Keys for which additional attributes may be gathered by querying a store which provides the specified attributes.
    fn pending_all(&self, fetchable: FileAttributes) -> Vec<Key> {
        if fetchable.none() {
            return vec![];
        }
        self.common
            .pending(fetchable, self.compute_aux_data)
            .map(|(key, _attrs)| key.clone())
            .collect()
    }

    /// Returns all incomplete requested Keys for which we haven't discovered an LFS pointer, and for which additional attributes may be gathered by querying a store which provides the specified attributes.
    fn pending_nonlfs(&self, fetchable: FileAttributes) -> Vec<Key> {
        if fetchable.none() {
            return vec![];
        }
        self.common
            .pending(fetchable, self.compute_aux_data)
            .map(|(key, _attrs)| key.clone())
            .filter(|k| !self.lfs_pointers.contains_key(k))
            .collect()
    }

    /// Returns all incomplete requested Keys as Store, with content Sha256 from the LFS pointer if available, for which additional attributes may be gathered by querying a store which provides the specified attributes
    fn pending_storekey(&self, fetchable: FileAttributes) -> Vec<StoreKey> {
        if fetchable.none() {
            return vec![];
        }
        self.common
            .pending(fetchable, self.compute_aux_data)
            .map(|(key, _attrs)| key.clone())
            .map(|k| self.storekey(k))
            .collect()
    }

    /// Returns the Key as a StoreKey, as a StoreKey::Content with Sha256 from the LFS Pointer, if available, otherwise as a StoreKey::HgId.
    /// Every StoreKey returned from this function is guaranteed to have an associated Key, so unwrapping is fine.
    fn storekey(&self, key: Key) -> StoreKey {
        if let Some((ptr, _)) = self.lfs_pointers.get(&key) {
            StoreKey::Content(ContentHash::Sha256(ptr.sha256()), Some(key))
        } else {
            StoreKey::HgId(key)
        }
    }

    #[instrument(level = "debug", skip(self))]
    fn mark_complete(&mut self, key: &Key) {
        if let Some((ptr, _)) = self.lfs_pointers.remove(key) {
            self.pointer_origin.write().remove(&ptr.sha256());
        }
    }

    #[instrument(level = "debug", skip(self, ptr))]
    fn found_pointer(&mut self, key: Key, ptr: LfsPointersEntry, typ: StoreType, write: bool) {
        let sha256 = ptr.sha256();
        // Overwrite StoreType::Local with StoreType::Shared, but not vice versa
        match typ {
            StoreType::Shared => {
                self.pointer_origin.write().insert(sha256, typ);
            }
            StoreType::Local => {
                self.pointer_origin.write().entry(sha256).or_insert(typ);
            }
        }
        self.lfs_pointers.insert(key, (ptr, write));
    }

    #[instrument(level = "debug", skip(self, sf))]
    fn found_attributes(&mut self, key: Key, sf: StoreFile, typ: Option<StoreType>) {
        self.key_origin
            .insert(key.clone(), typ.unwrap_or(StoreType::Shared));

        if self.common.found(key.clone(), sf) {
            self.mark_complete(&key);
        }
    }

    #[instrument(level = "trace", skip(file, indexedlog_cache, memcache), fields(memcache = memcache.is_some()))]
    fn evict_to_cache(
        key: Key,
        file: LazyFile,
        indexedlog_cache: &IndexedLogHgIdDataStore,
        memcache: Option<Arc<MemcacheStore>>,
    ) -> Result<LazyFile> {
        let cache_entry = file.indexedlog_cache_entry(key.clone())?.ok_or_else(|| {
                anyhow!("expected LazyFile::EdenApi or LazyFile::Memcache, other LazyFile variants should not be written to cache")
            })?;
        if let Some(memcache) = memcache.as_ref() {
            memcache.add_mcdata(cache_entry.clone().try_into()?);
        }
        indexedlog_cache.put_entry(cache_entry)?;
        let mmap_entry = indexedlog_cache
            .get_entry(key)?
            .ok_or_else(|| anyhow!("failed to read entry back from indexedlog after writing"))?;
        Ok(LazyFile::IndexedLog(mmap_entry))
    }

    #[instrument(level = "debug", skip(self, entry))]
    fn found_indexedlog(&mut self, key: Key, entry: Entry, typ: StoreType) {
        if entry.metadata().is_lfs() {
            if self.extstored_policy == ExtStoredPolicy::Use {
                match entry.try_into() {
                    Ok(ptr) => self.found_pointer(key, ptr, typ, true),
                    Err(err) => self.errors.keyed_error(key, err),
                }
            }
        } else {
            self.found_attributes(key, LazyFile::IndexedLog(entry).into(), Some(typ))
        }
    }

    #[instrument(skip(self, store))]
    pub(crate) fn fetch_indexedlog(&mut self, store: &IndexedLogHgIdDataStore, typ: StoreType) {
        let pending = self.pending_nonlfs(FileAttributes::CONTENT);
        if pending.is_empty() {
            return;
        }
        self.metrics.indexedlog.store(typ).fetch(pending.len());
        for key in pending.into_iter() {
            let res = store.get_raw_entry(&key);
            match res {
                Ok(Some(entry)) => {
                    self.metrics.indexedlog.store(typ).hit(1);
                    self.found_indexedlog(key, entry, typ)
                }
                Ok(None) => {
                    self.metrics.indexedlog.store(typ).miss(1);
                }
                Err(err) => {
                    self.metrics.indexedlog.store(typ).err(1);
                    self.errors.keyed_error(key, err)
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self, entry))]
    fn found_aux_indexedlog(&mut self, key: Key, entry: AuxDataEntry, typ: StoreType) {
        let aux_data: FileAuxData = entry.into();
        self.found_attributes(key, aux_data.into(), Some(typ));
    }

    #[instrument(skip(self, store))]
    pub(crate) fn fetch_aux_indexedlog(&mut self, store: &AuxStore, typ: StoreType) {
        let pending = self.pending_all(FileAttributes::AUX);
        if pending.is_empty() {
            return;
        }
        self.metrics.aux.store(typ).fetch(pending.len());

        for key in pending.into_iter() {
            let res = store.get(key.hgid);
            match res {
                Ok(Some(aux)) => {
                    self.metrics.aux.store(typ).hit(1);
                    self.found_aux_indexedlog(key, aux, typ)
                }
                Ok(None) => {
                    self.metrics.aux.store(typ).miss(1);
                }
                Err(err) => {
                    self.metrics.aux.store(typ).err(1);
                    self.errors.keyed_error(key, err)
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self, entry))]
    fn found_lfs(&mut self, key: Key, entry: LfsStoreEntry, typ: StoreType) {
        match entry {
            LfsStoreEntry::PointerAndBlob(ptr, blob) => {
                self.found_attributes(key, LazyFile::Lfs(blob, ptr).into(), Some(typ))
            }
            LfsStoreEntry::PointerOnly(ptr) => self.found_pointer(key, ptr, typ, false),
        }
    }

    #[instrument(skip(self, store))]
    pub(crate) fn fetch_lfs(&mut self, store: &LfsStore, typ: StoreType) {
        let pending = self.pending_storekey(FileAttributes::CONTENT);
        if pending.is_empty() {
            return;
        }
        self.metrics.lfs.store(typ).fetch(pending.len());
        for store_key in pending.into_iter() {
            let key = store_key.clone().maybe_into_key().expect(
                "no Key present in StoreKey, even though this should be guaranteed by pending_all",
            );
            match store.fetch_available(&store_key) {
                Ok(Some(entry)) => {
                    // TODO(meyer): Make found behavior w/r/t LFS pointers and content consistent
                    self.metrics.lfs.store(typ).hit(1);
                    self.found_lfs(key, entry, typ)
                }
                Ok(None) => {
                    self.metrics.lfs.store(typ).miss(1);
                }
                Err(err) => {
                    self.metrics.lfs.store(typ).err(1);
                    self.errors.keyed_error(key, err)
                }
            }
        }
    }

    #[instrument(level = "debug", skip(self, entry, indexedlog_cache))]
    fn found_memcache(
        &mut self,
        entry: McData,
        indexedlog_cache: Option<&IndexedLogHgIdDataStore>,
    ) {
        let key = entry.key.clone();
        if entry.metadata.is_lfs() {
            match entry.try_into() {
                Ok(ptr) => self.found_pointer(key, ptr, StoreType::Shared, true),
                Err(err) => self.errors.keyed_error(key, err),
            }
        } else if let Some(indexedlog_cache) = indexedlog_cache {
            match Self::evict_to_cache(
                key.clone(),
                LazyFile::Memcache(entry),
                indexedlog_cache,
                None,
            ) {
                Ok(cached) => {
                    self.found_attributes(key, cached.into(), None);
                }
                Err(err) => self.errors.keyed_error(key, err),
            }
        } else {
            self.found_attributes(key, LazyFile::Memcache(entry).into(), None);
        }
    }

    fn fetch_memcache_inner(
        &mut self,
        store: &MemcacheStore,
        indexedlog_cache: Option<&IndexedLogHgIdDataStore>,
    ) -> Result<()> {
        let pending = self.pending_nonlfs(FileAttributes::CONTENT);
        if pending.is_empty() {
            return Ok(());
        }
        self.fetch_logger
            .as_ref()
            .map(|fl| fl.report_keys(pending.iter()));

        for res in store.get_data_iter(&pending)?.into_iter() {
            match res {
                Ok(mcdata) => self.found_memcache(mcdata, indexedlog_cache),
                Err(err) => self.errors.other_error(err),
            }
        }
        Ok(())
    }

    #[instrument(skip(self, store, indexedlog_cache))]
    pub(crate) fn fetch_memcache(
        &mut self,
        store: &MemcacheStore,
        indexedlog_cache: Option<&IndexedLogHgIdDataStore>,
    ) {
        if let Err(err) = self.fetch_memcache_inner(store, indexedlog_cache) {
            self.errors.other_error(err);
        }
    }

    #[instrument(
        level = "debug",
        skip(entry, indexedlog_cache, lfs_cache, aux_cache, memcache)
    )]
    fn found_edenapi(
        entry: FileEntry,
        indexedlog_cache: Option<Arc<IndexedLogHgIdDataStore>>,
        lfs_cache: Option<Arc<LfsStore>>,
        aux_cache: Option<Arc<AuxStore>>,
        memcache: Option<Arc<MemcacheStore>>,
    ) -> Result<(StoreFile, Option<LfsPointersEntry>)> {
        let key = entry.key.clone();
        let mut file = StoreFile::default();
        let mut lfsptr = None;

        if let Some(aux_data) = entry.aux_data() {
            let aux_data: FileAuxData = aux_data.clone().into();
            if let Some(aux_cache) = aux_cache.as_ref() {
                aux_cache.put(key.hgid, &aux_data.into())?;
            }
            file.aux_data = Some(aux_data);
        }

        if let Some(content) = entry.content() {
            if content.metadata().is_lfs() {
                let ptr: LfsPointersEntry = entry.try_into()?;
                if let Some(lfs_cache) = lfs_cache.as_ref() {
                    lfs_cache.add_pointer(ptr.clone())?;
                }
                lfsptr = Some(ptr);
            } else if let Some(indexedlog_cache) = indexedlog_cache.as_ref() {
                file.content = Some(Self::evict_to_cache(
                    key,
                    LazyFile::EdenApi(entry),
                    indexedlog_cache,
                    memcache,
                )?);
            } else {
                file.content = Some(LazyFile::EdenApi(entry));
            }
        }

        Ok((file, lfsptr))
    }

    fn fetch_edenapi_inner(
        &mut self,
        store: &EdenApiFileStore,
        indexedlog_cache: Option<Arc<IndexedLogHgIdDataStore>>,
        lfs_cache: Option<Arc<LfsStore>>,
        aux_cache: Option<Arc<AuxStore>>,
        memcache: Option<Arc<MemcacheStore>>,
    ) -> Result<()> {
        let fetchable = FileAttributes::CONTENT | FileAttributes::AUX;
        let span = tracing::info_span!(
            "fetch_edenapi",
            downloaded = field::Empty,
            uploaded = field::Empty,
            requests = field::Empty,
            time = field::Empty,
            latency = field::Empty,
            download_speed = field::Empty,
            scmstore = true,
        );
        let _enter = span.enter();

        let pending = self.pending_nonlfs(fetchable);
        if pending.is_empty() {
            return Ok(());
        }
        self.fetch_logger
            .as_ref()
            .map(|fl| fl.report_keys(pending.iter()));

        // TODO(meyer): Iterators or otherwise clean this up
        let pending_attrs: Vec<_> = pending
            .into_iter()
            .map(|k| {
                let actionable = self.common.actionable(&k, fetchable, self.compute_aux_data);
                FileSpec {
                    key: k,
                    attrs: actionable.into(),
                }
            })
            .collect();

        let response = block_on(store.files_attrs(pending_attrs))?;
        let entries = response
            .entries
            .map(move |res_entry| {
                let lfs_cache = lfs_cache.clone();
                let indexedlog_cache = indexedlog_cache.clone();
                let aux_cache = aux_cache.clone();
                let memcache = memcache.clone();
                spawn_blocking(move || {
                    res_entry.map(move |entry| {
                        (
                            entry.key.clone(),
                            Self::found_edenapi(
                                entry,
                                indexedlog_cache,
                                lfs_cache,
                                aux_cache,
                                memcache,
                            ),
                        )
                    })
                })

                // Processing a response may involve compressing the response, which
                // can be expensive. If we don't process entries fast enough, edenapi
                // can start queueing up responses which causes forever increasing
                // memory usage. So let's process responses in parallel to stay ahead
                // of download speeds.
            })
            .buffer_unordered(4);

        // Record found entries
        for res in stream_to_iter(entries) {
            // TODO(meyer): This outer EdenApi error with no key sucks
            let (key, res) = res??;
            match res {
                Ok((file, maybe_lfsptr)) => {
                    if let Some(lfsptr) = maybe_lfsptr {
                        self.found_pointer(key.clone(), lfsptr, StoreType::Shared, false);
                    }
                    self.found_attributes(key, file, Some(StoreType::Shared));
                }
                Err(err) => self.errors.keyed_error(key, err),
            }
        }

        util::record_edenapi_stats(&span, &block_on(response.stats)?);

        Ok(())
    }

    pub(crate) fn fetch_edenapi(
        &mut self,
        store: &EdenApiFileStore,
        indexedlog_cache: Option<Arc<IndexedLogHgIdDataStore>>,
        lfs_cache: Option<Arc<LfsStore>>,
        aux_cache: Option<Arc<AuxStore>>,
        memcache: Option<Arc<MemcacheStore>>,
    ) {
        if let Err(err) =
            self.fetch_edenapi_inner(store, indexedlog_cache, lfs_cache, aux_cache, memcache)
        {
            self.errors.other_error(err);
        }
    }

    fn fetch_lfs_remote_inner(
        &mut self,
        store: &LfsRemoteInner,
        local: Option<Arc<LfsStore>>,
        cache: Option<Arc<LfsStore>>,
    ) -> Result<()> {
        let errors = &mut self.errors;
        let pending: HashSet<_> = self
            .lfs_pointers
            .iter()
            .map(|(key, (ptr, write))| {
                if *write {
                    if let Some(lfs_cache) = cache.as_ref() {
                        if let Err(err) = lfs_cache.add_pointer(ptr.clone()) {
                            errors.keyed_error(key.clone(), err);
                        }
                    }
                }
                (ptr.sha256(), ptr.size() as usize)
            })
            .collect();
        if pending.is_empty() {
            return Ok(());
        }
        self.fetch_logger
            .as_ref()
            .map(|fl| fl.report_keys(self.lfs_pointers.keys()));

        // Fetch & write to local LFS stores
        store.batch_fetch(&pending, {
            let lfs_local = local.clone();
            let lfs_cache = cache.clone();
            let pointer_origin = self.pointer_origin.clone();
            move |sha256, data| -> Result<()> {
                match pointer_origin.read().get(&sha256).ok_or_else(|| {
                    anyhow!(
                        "no source found for Sha256; received unexpected Sha256 from LFS server"
                    )
                })? {
                    StoreType::Local => lfs_local
                        .as_ref()
                        .expect("no lfs_local present when handling local LFS pointer")
                        .add_blob(&sha256, data),
                    StoreType::Shared => lfs_cache
                        .as_ref()
                        .expect("no lfs_cache present when handling cache LFS pointer")
                        .add_blob(&sha256, data),
                }
            }
        })?;

        // After prefetching into the local LFS stores, retry fetching from them. The returned Bytes will then be mmaps rather
        // than large files stored in memory.
        // TODO(meyer): We probably want to intermingle this with the remote fetch handler to avoid files being evicted between there
        // and here, rather than just retrying the local fetches.
        if let Some(ref lfs_cache) = cache {
            self.fetch_lfs(lfs_cache, StoreType::Shared)
        }

        if let Some(ref lfs_local) = local {
            self.fetch_lfs(lfs_local, StoreType::Local)
        }

        Ok(())
    }

    #[instrument(skip(self, store, local, cache), fields(local = local.is_some(), cache = cache.is_some()))]
    pub(crate) fn fetch_lfs_remote(
        &mut self,
        store: &LfsRemoteInner,
        local: Option<Arc<LfsStore>>,
        cache: Option<Arc<LfsStore>>,
    ) {
        if let Err(err) = self.fetch_lfs_remote_inner(store, local, cache) {
            self.errors.other_error(err);
        }
    }

    #[instrument(level = "debug", skip(self, bytes))]
    fn found_contentstore(&mut self, key: Key, bytes: Vec<u8>, meta: Metadata) {
        if meta.is_lfs() {
            self.metrics.contentstore.hit_lfsptr(1);
            // Do nothing. We're trying to avoid exposing LFS pointers to the consumer of this API.
            // We very well may need to expose LFS Pointers to the caller in the end (to match ContentStore's
            // ExtStoredPolicy behavior), but hopefully not, and if so we'll need to make it type safe.
            tracing::warn!("contentstore fallback returned serialized lfs pointer");
        } else {
            tracing::warn!(
                "contentstore fetched a file scmstore couldn't, \
                this indicates a bug or unsupported configuration: \
                fetched key '{}', found {} bytes of content with metadata {:?}.",
                key,
                bytes.len(),
                meta,
            );
            self.metrics.contentstore.hit(1);
            self.found_attributes(key, LazyFile::ContentStore(bytes.into(), meta).into(), None)
        }
    }

    fn fetch_contentstore_inner(
        &mut self,
        store: &ContentStore,
        pending: &mut Vec<StoreKey>,
    ) -> Result<()> {
        store.prefetch(&pending)?;
        for store_key in pending.drain(..) {
            let key = store_key.clone().maybe_into_key().expect(
                "no Key present in StoreKey, even though this should be guaranteed by pending_storekey",
            );
            // Using the ContentStore API, fetch the hg file blob, then, if it's found, also fetch the file metadata.
            // Returns the requested file as Result<(Option<Vec<u8>>, Option<Metadata>)>
            // Produces a Result::Err if either the blob or metadata get returned an error
            let res = store
                .get(store_key.clone())
                .map(|store_result| store_result.into())
                .and_then({
                    let store_key = store_key.clone();
                    |maybe_blob| {
                        Ok((
                            maybe_blob,
                            store
                                .get_meta(store_key)
                                .map(|store_result| store_result.into())?,
                        ))
                    }
                });

            match res {
                Ok((Some(blob), Some(meta))) => self.found_contentstore(key, blob, meta),
                Err(err) => {
                    self.metrics.contentstore.err(1);
                    self.errors.keyed_error(key, err)
                }
                _ => {
                    self.metrics.contentstore.miss(1);
                }
            }
        }

        Ok(())
    }

    #[instrument(skip(self, store))]
    pub(crate) fn fetch_contentstore(&mut self, store: &ContentStore) {
        let mut pending = self.pending_storekey(FileAttributes::CONTENT);
        if pending.is_empty() {
            return;
        }
        self.metrics.contentstore.fetch(pending.len());
        if let Err(err) = self.fetch_contentstore_inner(store, &mut pending) {
            self.errors.other_error(err);
            self.metrics.contentstore.err(pending.len());
        }
    }

    #[instrument(skip(self))]
    pub(crate) fn derive_computable(&mut self) {
        if !self.compute_aux_data {
            return;
        }

        for (key, value) in self.common.found.iter_mut() {
            let span = tracing::debug_span!("checking derivations", %key);
            let _guard = span.enter();

            let missing = self.common.request_attrs - value.attrs();
            let actionable = value.attrs().with_computable() & missing;

            if actionable.aux_data {
                tracing::debug!("computing aux data");
                if let Err(err) = value.compute_aux_data() {
                    self.errors.keyed_error(key.clone(), err);
                } else {
                    tracing::debug!("computed aux data");
                    self.computed_aux_data
                        .insert(key.clone(), self.key_origin[key]);
                }
            }

            // mark complete if applicable
            if value.attrs().has(self.common.request_attrs) {
                tracing::debug!("marking complete");
                // TODO(meyer): Extract out a "FetchPending" object like FetchErrors, or otherwise make it possible
                // to share a "mark complete" implementation while holding a mutable reference to self.found.
                self.common.pending.remove(key);
                if let Some((ptr, _)) = self.lfs_pointers.remove(key) {
                    self.pointer_origin.write().remove(&ptr.sha256());
                }
            }
        }
    }

    // TODO(meyer): Improve how local caching works. At the very least do this in the background.
    // TODO(meyer): Log errors here instead of just ignoring.
    #[instrument(
        skip(self, aux_cache, aux_local),
        fields(
            aux_cache = aux_cache.is_some(),
            aux_local = aux_local.is_some()))]
    pub(crate) fn write_to_cache(
        &mut self,
        aux_cache: Option<&AuxStore>,
        aux_local: Option<&AuxStore>,
    ) {
        {
            let span = tracing::trace_span!("computed");
            let _guard = span.enter();
            for (key, origin) in self.computed_aux_data.drain() {
                let entry: AuxDataEntry = self.common.found[&key].aux_data.unwrap().into();
                match origin {
                    StoreType::Shared => {
                        if let Some(ref aux_cache) = aux_cache {
                            let _ = aux_cache.put(key.hgid, &entry);
                        }
                    }
                    StoreType::Local => {
                        if let Some(ref aux_local) = aux_local {
                            let _ = aux_local.put(key.hgid, &entry);
                        }
                    }
                }
            }
        }
    }

    #[instrument(skip(self))]
    pub(crate) fn finish(self) -> FetchResults<StoreFile, FileStoreFetchMetrics> {
        self.common.results(self.errors, self.metrics)
    }
}
