/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "ObjectStore.h"

#include <folly/Conv.h>
#include <folly/Executor.h>
#include <folly/Format.h>
#include <folly/futures/Future.h>
#include <folly/io/IOBuf.h>

#include <stdexcept>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/BackingStore.h"
#include "eden/fs/store/LocalStore.h"
#include "eden/fs/store/ObjectFetchContext.h"
#include "eden/fs/telemetry/EdenStats.h"

using folly::Future;
using folly::makeFuture;
using std::shared_ptr;
using std::string;
using std::unique_ptr;

namespace facebook {
namespace eden {

namespace {
constexpr uint64_t kImportPriorityDeprioritizeAmount = 1;
}

std::shared_ptr<ObjectStore> ObjectStore::create(
    shared_ptr<LocalStore> localStore,
    shared_ptr<BackingStore> backingStore,
    shared_ptr<TreeCache> treeCache,
    shared_ptr<EdenStats> stats,
    folly::Executor::KeepAlive<folly::Executor> executor,
    std::shared_ptr<ProcessNameCache> processNameCache,
    std::shared_ptr<StructuredLogger> structuredLogger,
    std::shared_ptr<const EdenConfig> edenConfig) {
  return std::shared_ptr<ObjectStore>{new ObjectStore{
      std::move(localStore),
      std::move(backingStore),
      std::move(treeCache),
      std::move(stats),
      executor,
      processNameCache,
      structuredLogger,
      edenConfig}};
}

ObjectStore::ObjectStore(
    shared_ptr<LocalStore> localStore,
    shared_ptr<BackingStore> backingStore,
    shared_ptr<TreeCache> treeCache,
    shared_ptr<EdenStats> stats,
    folly::Executor::KeepAlive<folly::Executor> executor,
    std::shared_ptr<ProcessNameCache> processNameCache,
    std::shared_ptr<StructuredLogger> structuredLogger,
    std::shared_ptr<const EdenConfig> edenConfig)
    : metadataCache_{folly::in_place, kCacheSize},
      treeCache_{std::move(treeCache)},
      localStore_{std::move(localStore)},
      backingStore_{std::move(backingStore)},
      stats_{std::move(stats)},
      executor_{executor},
      pidFetchCounts_{std::make_unique<PidFetchCounts>()},
      processNameCache_(processNameCache),
      structuredLogger_(structuredLogger),
      edenConfig_(edenConfig) {}

ObjectStore::~ObjectStore() {}

void ObjectStore::updateProcessFetch(
    const ObjectFetchContext& fetchContext) const {
  if (auto pid = fetchContext.getClientPid()) {
    auto fetch_count = pidFetchCounts_->recordProcessFetch(pid.value());
    auto threshold = edenConfig_->fetchHeavyThreshold.getValue();
    if (fetch_count && threshold && !(fetch_count % threshold)) {
      sendFetchHeavyEvent(pid.value(), fetch_count);
    }
  }
}

void ObjectStore::sendFetchHeavyEvent(pid_t pid, uint64_t fetch_count) const {
  auto processName = processNameCache_->getSpacedProcessName(pid);
  if (processName.has_value()) {
    structuredLogger_->logEvent(
        FetchHeavy{processName.value(), pid, fetch_count});
  }
}

void ObjectStore::deprioritizeWhenFetchHeavy(
    ObjectFetchContext& context) const {
  auto pid = context.getClientPid();
  if (pid.has_value()) {
    auto fetch_count = pidFetchCounts_->getCountByPid(pid.value());
    auto threshold = edenConfig_->fetchHeavyThreshold.getValue();
    if (threshold && fetch_count >= threshold) {
      context.deprioritize(kImportPriorityDeprioritizeAmount);
    }
  }
}

RootId ObjectStore::parseRootId(folly::StringPiece rootId) {
  return backingStore_->parseRootId(rootId);
}

std::string ObjectStore::renderRootId(const RootId& rootId) {
  return backingStore_->renderRootId(rootId);
}

Future<shared_ptr<const Tree>> ObjectStore::getRootTree(
    const RootId& rootId,
    ObjectFetchContext& context) const {
  XLOG(DBG3) << "getRootTree(" << rootId << ")";

  return backingStore_->getRootTree(rootId, context)
      .via(executor_)
      .thenValue([treeCache = treeCache_,
                  rootId,
                  localStore = localStore_,
                  edenConfig = edenConfig_](std::shared_ptr<const Tree> tree) {
        if (!tree) {
          throw std::domain_error(
              folly::to<string>("unable to import root ", rootId));
        }

        localStore->putTree(tree.get());
        treeCache->insert(tree);

        return tree;
      });
}

Future<shared_ptr<const Tree>> ObjectStore::getTree(
    const Hash& id,
    ObjectFetchContext& fetchContext) const {
  // Check in the LocalStore first

  // TODO: We should consider checking if we have in flight BackingStore
  // requests on this layer instead of only in the BackingStore. Consider the
  // case in which thread A and thread B both request a Tree at the same time.
  // Let's say thread A checks the LocalStore, then thread B checks the
  // LocalStore, gets the file from the BackingStore (making a request to the
  // server), then writes the Tree to the LocalStore. Now when thread A checks
  // for in flight requests in the BackingStore, it will not see any since
  // thread B has completely finished, so thread A will make a duplicate
  // request. If we we're to mark here that we got a request on this layer, then
  // we could avoid that case.

  if (auto maybeTree = treeCache_->get(id)) {
    fetchContext.didFetch(
        ObjectFetchContext::Tree, id, ObjectFetchContext::FromMemoryCache);

    updateProcessFetch(fetchContext);

    return maybeTree;
  }

  return localStore_->getTree(id).thenValue([self = shared_from_this(),
                                             id,
                                             &fetchContext](
                                                shared_ptr<const Tree> tree) {
    if (tree) {
      XLOG(DBG4) << "tree " << id << " found in local store";
      fetchContext.didFetch(
          ObjectFetchContext::Tree, id, ObjectFetchContext::FromDiskCache);

      self->updateProcessFetch(fetchContext);
      self->treeCache_->insert(tree);
      return makeFuture(std::move(tree));
    }

    self->deprioritizeWhenFetchHeavy(fetchContext);

    // Load the tree from the BackingStore.
    return self->backingStore_->getTree(id, fetchContext)
        .via(self->executor_)
        .thenValue([self, id, &fetchContext, localStore = self->localStore_](
                       unique_ptr<const Tree> tree) {
          if (!tree) {
            // TODO: Perhaps we should do some short-term negative
            // caching?
            XLOG(DBG2) << "unable to find tree " << id;
            throw std::domain_error(
                folly::to<string>("tree ", id.toString(), " not found"));
          }

          // promote to shared_ptr so we can store in the cache and return
          std::shared_ptr<const Tree> loadedTree{std::move(tree)};
          localStore->putTree(loadedTree.get());
          self->treeCache_->insert(loadedTree);
          XLOG(DBG3) << "tree " << id << " retrieved from backing store";
          fetchContext.didFetch(
              ObjectFetchContext::Tree,
              id,
              ObjectFetchContext::FromBackingStore);

          self->updateProcessFetch(fetchContext);
          return shared_ptr<const Tree>(std::move(loadedTree));
        });
  });
}

folly::Future<folly::Unit> ObjectStore::prefetchBlobs(
    const std::vector<Hash>& ids,
    ObjectFetchContext& fetchContext) const {
  // In theory we could/should ask the localStore_ to filter the list
  // of ids down to just the set that we need to load, but there is no
  // bulk key existence check in rocksdb, so we would need to cause it
  // to load all the blocks of those keys into memory.
  // So for the moment we are committing a layering violation in the
  // interest of making things faster in practice by just asking the
  // mercurial backing store to ensure that its local hgcache storage
  // has entries for all of the requested keys.
  if (ids.empty()) {
    return folly::unit;
  }
  return backingStore_->prefetchBlobs(ids, fetchContext).via(executor_);
}

Future<shared_ptr<const Blob>> ObjectStore::getBlob(
    const Hash& id,
    ObjectFetchContext& fetchContext) const {
  auto self = shared_from_this();

  return localStore_->getBlob(id).thenValue([id, &fetchContext, self](
                                                shared_ptr<const Blob> blob) {
    if (blob) {
      // Not computing the BlobMetadata here because if the blob was found
      // in the local store, the LocalStore probably also has the metadata
      // already, and the caller may not even need the SHA-1 here. (If the
      // caller needed the SHA-1, they would have called getBlobMetadata
      // instead.)
      XLOG(DBG4) << "blob " << id << " found in local store";
      self->updateBlobStats(true, false);
      fetchContext.didFetch(
          ObjectFetchContext::Blob, id, ObjectFetchContext::FromDiskCache);

      self->updateProcessFetch(fetchContext);
      return makeFuture(shared_ptr<const Blob>(std::move(blob)));
    }

    self->deprioritizeWhenFetchHeavy(fetchContext);

    // Look in the BackingStore
    return self->backingStore_->getBlob(id, fetchContext)
        .via(self->executor_)
        .thenValue([self, &fetchContext, id](
                       unique_ptr<const Blob> loadedBlob) {
          if (loadedBlob) {
            XLOG(DBG3) << "blob " << id << "  retrieved from backing store";
            self->updateBlobStats(false, true);
            fetchContext.didFetch(
                ObjectFetchContext::Blob,
                id,
                ObjectFetchContext::FromBackingStore);

            self->updateProcessFetch(fetchContext);

            auto metadata = self->localStore_->putBlob(id, loadedBlob.get());
            self->metadataCache_.wlock()->set(id, metadata);
            return shared_ptr<const Blob>(std::move(loadedBlob));
          }

          XLOG(DBG2) << "unable to find blob " << id;
          self->updateBlobStats(false, false);
          // TODO: Perhaps we should do some short-term negative caching?
          throw std::domain_error(
              folly::to<string>("blob ", id.toString(), " not found"));
        });
  });
}

void ObjectStore::updateBlobStats(bool local, bool backing) const {
  ObjectStoreThreadStats& stats = stats_->getObjectStoreStatsForCurrentThread();
  stats.getBlobFromLocalStore.addValue(local);
  stats.getBlobFromBackingStore.addValue(backing);
}

Future<BlobMetadata> ObjectStore::getBlobMetadata(
    const Hash& id,
    ObjectFetchContext& context) const {
  // Check in-memory cache
  {
    auto metadataCache = metadataCache_.wlock();
    auto cacheIter = metadataCache->find(id);
    if (cacheIter != metadataCache->end()) {
      updateBlobMetadataStats(true, false, false);
      context.didFetch(
          ObjectFetchContext::BlobMetadata,
          id,
          ObjectFetchContext::FromMemoryCache);

      updateProcessFetch(context);
      return cacheIter->second;
    }
  }

  auto self = shared_from_this();

  // Check local store
  return localStore_->getBlobMetadata(id).thenValue(
      [self, id, &context](std::optional<BlobMetadata>&& metadata) {
        if (metadata) {
          self->updateBlobMetadataStats(false, true, false);
          self->metadataCache_.wlock()->set(id, *metadata);
          context.didFetch(
              ObjectFetchContext::BlobMetadata,
              id,
              ObjectFetchContext::FromDiskCache);

          self->updateProcessFetch(context);
          return makeFuture(*metadata);
        }

        self->deprioritizeWhenFetchHeavy(context);

        // Check backing store
        //
        // TODO: It would be nice to add a smarter API to the BackingStore so
        // that we can query it just for the blob metadata if it supports
        // getting that without retrieving the full blob data.
        //
        // TODO: This should probably check the LocalStore for the blob first,
        // especially when we begin to expire entries in RocksDB.
        return self->backingStore_->getBlob(id, context)
            .via(self->executor_)
            .thenValue([self, id, &context](std::unique_ptr<Blob> blob) {
              if (blob) {
                self->updateBlobMetadataStats(false, false, true);
                auto metadata = self->localStore_->putBlob(id, blob.get());
                self->metadataCache_.wlock()->set(id, metadata);
                // I could see an argument for recording this fetch with
                // type Blob instead of BlobMetadata, but it's probably more
                // useful in context to know how many metadata fetches
                // occurred. Also, since backing stores don't directly
                // support fetching metadata, it should be clear.
                context.didFetch(
                    ObjectFetchContext::BlobMetadata,
                    id,
                    ObjectFetchContext::FromBackingStore);

                self->updateProcessFetch(context);
                return makeFuture(metadata);
              }

              self->updateBlobMetadataStats(false, false, false);
              throw std::domain_error(
                  folly::to<string>("blob ", id.toString(), " not found"));
            });
      });
}

void ObjectStore::updateBlobMetadataStats(bool memory, bool local, bool backing)
    const {
  ObjectStoreThreadStats& stats = stats_->getObjectStoreStatsForCurrentThread();
  stats.getBlobMetadataFromMemory.addValue(memory);
  stats.getBlobMetadataFromLocalStore.addValue(local);
  stats.getBlobMetadataFromBackingStore.addValue(backing);
}

Future<Hash> ObjectStore::getBlobSha1(
    const Hash& id,
    ObjectFetchContext& context) const {
  return getBlobMetadata(id, context)
      .thenValue([](const BlobMetadata& metadata) { return metadata.sha1; });
}

Future<uint64_t> ObjectStore::getBlobSize(
    const Hash& id,
    ObjectFetchContext& context) const {
  return getBlobMetadata(id, context)
      .thenValue([](const BlobMetadata& metadata) { return metadata.size; });
}
} // namespace eden
} // namespace facebook
