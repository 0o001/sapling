/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once

#include <memory>
#include "eden/fs/store/IObjectStore.h"

namespace facebook {
namespace eden {

class BackingStore;
class Blob;
class Hash;
class LocalStore;
class Tree;

/**
 * ObjectStore is a content-addressed store for eden object data.
 *
 * The ObjectStore class itself is primarily a wrapper around two other
 * underlying storage types:
 * - LocalStore, which caches object data locally in a RocksDB instance
 * - BackingStore, which represents the authoritative source for the object
 *   data.  The BackingStore is generally more expensive to query for object
 *   data, and may not be available during offline operation.
 */
class ObjectStore : public IObjectStore {
 public:
  ObjectStore(
      std::shared_ptr<LocalStore> localStore,
      std::shared_ptr<BackingStore> backingStore);
  virtual ~ObjectStore();

  /**
   * Get a Tree by ID.
   *
   * This function never returns nullptr.  It throws std::domain_error if the
   * specified tree ID does not exist, or possibly other exceptions on error.
   *
   * TODO: This API will be deprecated in favor of getTreeFuture()
   */
  std::unique_ptr<Tree> getTree(const Hash& id) const override;

  /**
   * Get a Blob by ID.
   *
   * This function never returns nullptr.  It throws std::domain_error if the
   * specified blob ID does not exist, or possibly other exceptions on error.
   *
   * TODO: This API will be deprecated in favor of getBlobFuture()
   */
  std::unique_ptr<Blob> getBlob(const Hash& id) const override;

  /**
   * Get a Tree by commit ID.
   *
   * This throws std::domain_error if the specified blob ID does not exist, or
   * possibly other exceptions on error.
   *
   * TODO: This API will be deprecated in favor of getBlobMetadata()
   */
  Hash getSha1ForBlob(const Hash& id) const override;

  /**
   * Get a Tree by ID.
   *
   * This returns a Future object that will produce the Tree when it is ready.
   * It may result in a std::domain_error if the specified tree ID does not
   * exist, or possibly other exceptions on error.
   */
  folly::Future<std::unique_ptr<Tree>> getTreeFuture(
      const Hash& id) const override;

  /**
   * Get a Blob by ID.
   *
   * This returns a Future object that will produce the Blob when it is ready.
   * It may result in a std::domain_error if the specified blob ID does not
   * exist, or possibly other exceptions on error.
   */
  folly::Future<std::unique_ptr<Blob>> getBlobFuture(
      const Hash& id) const override;

  /**
   * Get a commit's root Tree.
   *
   * This returns a Future object that will produce the root Tree when it is
   * ready.  It may result in a std::domain_error if the specified commit ID
   * does not exist, or possibly other exceptions on error.
   */
  folly::Future<std::unique_ptr<Tree>> getTreeForCommit(
      const Hash& commitID) const override;

  /**
   * Get metadata about a Blob.
   *
   * This returns a Future object that will produce the BlobMetadata when it is
   * ready.  It may result in a std::domain_error if the specified blob does
   * not exist, or possibly other exceptions on error.
   */
  folly::Future<BlobMetadata> getBlobMetadata(const Hash& id) const override;

  /**
   * Get the LocalStore used by this ObjectStore
   */
  const std::shared_ptr<LocalStore>& getLocalStore() const {
    return localStore_;
  }

  /**
   * Get the BackingStore used by this ObjectStore
   */
  const std::shared_ptr<BackingStore>& getBackingStore() const {
    return backingStore_;
  }

 private:
  // Forbidden copy constructor and assignment operator
  ObjectStore(ObjectStore const&) = delete;
  ObjectStore& operator=(ObjectStore const&) = delete;

  /*
   * The LocalStore.
   *
   * Multiple ObjectStores (for different mount points) may share the same
   * LocalStore.
   */
  std::shared_ptr<LocalStore> localStore_;
  /*
   * The BackingStore.
   *
   * Multiple ObjectStores may share the same BackingStore.
   */
  std::shared_ptr<BackingStore> backingStore_;
};
}
} // facebook::eden
