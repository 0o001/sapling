/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include "eden/fs/store/BackingStore.h"

namespace facebook::eden {

class BackingStore;
class LocalStore;
class EdenStats;

/**
 * Implementation of a BackingStore that caches the returned data from another
 * BackingStore onto the LocalStore.
 *
 * Reads will first attempt to read from the LocalStore, and will only read
 * from the underlying BackingStore if the data wasn't found in the LocalStore.
 *
 * This should be used for BackingStores that either do not have local caching
 * builtin, or when reading from this cache is significantly slower than
 * reading from the LocalStore.
 */
class LocalStoreCachedBackingStore : public BackingStore {
 public:
  LocalStoreCachedBackingStore(
      std::shared_ptr<BackingStore> backingStore,
      std::shared_ptr<LocalStore> localStore,
      std::shared_ptr<EdenStats> stats);

  folly::SemiFuture<std::unique_ptr<Tree>> getRootTree(
      const RootId& rootId,
      ObjectFetchContext& context) override;

  folly::SemiFuture<std::unique_ptr<TreeEntry>> getTreeEntryForRootId(
      const RootId& rootId,
      TreeEntryType treeEntryType,
      facebook::eden::PathComponentPiece pathComponentPiece,
      ObjectFetchContext& context) override;
  folly::SemiFuture<GetTreeRes> getTree(
      const Hash& id,
      ObjectFetchContext& context) override;
  folly::SemiFuture<GetBlobRes> getBlob(
      const Hash& id,
      ObjectFetchContext& context) override;

  FOLLY_NODISCARD folly::SemiFuture<folly::Unit> prefetchBlobs(
      HashRange ids,
      ObjectFetchContext& context) override;

  void periodicManagementTask() override;

  void startRecordingFetch() override;
  std::unordered_set<std::string> stopRecordingFetch() override;

  folly::SemiFuture<folly::Unit> importManifestForRoot(
      const RootId& rootId,
      const Hash& manifest) override;

  RootId parseRootId(folly::StringPiece rootId) override;
  std::string renderRootId(const RootId& rootId) override;

  std::optional<folly::StringPiece> getRepoName() override;

 private:
  std::shared_ptr<BackingStore> backingStore_;
  std::shared_ptr<LocalStore> localStore_;
  std::shared_ptr<EdenStats> stats_;
};

} // namespace facebook::eden
