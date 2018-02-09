/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once

#include <folly/Range.h>
#include <memory>
#include "eden/fs/rocksdb/RocksHandles.h"
#include "eden/fs/store/BlobMetadata.h"
#include "eden/fs/utils/PathFuncs.h"

namespace folly {
template <typename T>
class Optional;
}

namespace facebook {
namespace eden {

class Blob;
class Hash;
class StoreResult;
class Tree;

/*
 * LocalStore stores objects (trees and blobs) locally on disk.
 *
 * This is a content-addressed store, so objects can be only retrieved using
 * their hash.
 *
 * The LocalStore was originally only a cache.  The intent was that If an
 * object is not found in the LocalStore then it will need to be retrieved
 * from the BackingStore.  The introduction of HgProxyHashFamily renders this
 * comment a little inaccurate because we don't have a way to produce the
 * required data if the proxy hash data has been removed.  We expect things
 * to revert back to a more pure cache as we evolve our interfaces with
 * Mercurial and Mononoke.
 *
 * LocalStore is thread-safe, and can be used from multiple threads without
 * requiring the caller to perform locking around accesses to the LocalStore.
 */
class LocalStore {
 public:
  virtual ~LocalStore();

  /**
   * Which key space (and thus column family for the RocksDbLocalStore)
   * should be used to store a specific key.  The values of these are
   * coupled to the ordering of the columnFamilies descriptor in
   * RocksDbLocalStore.cpp and tableNames in SqliteLocalStore.cpp */
  enum KeySpace {
    /* 0 is the default column family, which we are not using */
    BlobFamily = 1,
    BlobMetaDataFamily = 2,
    TreeFamily = 3,
    HgProxyHashFamily = 4,
    HgCommitToTreeFamily = 5,

    End, // must be last!
  };

  /**
   * Close the underlying store.
   */
  virtual void close() = 0;

  /**
   * Get arbitrary unserialized data from the store.
   *
   * StoreResult::isValid() will be true if the key was found, and false
   * if the key was not present.
   *
   * May throw exceptions on error.
   */
  virtual StoreResult get(KeySpace keySpace, folly::ByteRange key) const = 0;
  StoreResult get(KeySpace keySpace, const Hash& id) const;

  /**
   * Get a Tree from the store.
   *
   * Returns nullptr if this key is not present in the store.
   * May throw exceptions on error (e.g., if this ID refers to a non-tree
   * object).
   */
  std::unique_ptr<Tree> getTree(const Hash& id) const;

  /**
   * Get a Blob from the store.
   *
   * Blob objects store file data.
   *
   * Returns nullptr if this key is not present in the store.
   * May throw exceptions on error (e.g., if this ID refers to a non-blob
   * object).
   */
  std::unique_ptr<Blob> getBlob(const Hash& id) const;

  /**
   * Get the size of a blob and the SHA-1 hash of its contents.
   *
   * Returns folly::none if this key is not present in the store, or throws an
   * exception on error.
   */
  folly::Optional<BlobMetadata> getBlobMetadata(const Hash& id) const;

  /**
   * Compute the serialized version of the tree.
   * Returns the key and the (not coalesced) serialized data.
   * This does not modify the contents of the store; it is the method
   * used by the putTree method to compute the data that it stores.
   * This is useful when computing the overall set of data during a
   * two phase import. */
  static std::pair<Hash, folly::IOBuf> serializeTree(const Tree* tree);

  /**
   * Test whether the key is stored.
   */
  virtual bool hasKey(KeySpace keySpace, folly::ByteRange key) const = 0;
  bool hasKey(KeySpace keySpace, const Hash& id) const;

  /**
   * Store a Tree into the TreeFamily KeySpace.
   *
   * Returns the Hash that can be used to look up the tree later.
   */
  Hash putTree(const Tree* tree);

  /**
   * Store a Blob.
   *
   * Returns a BlobMetadata about the blob, which includes the SHA-1 hash of
   * its contents.
   */
  BlobMetadata putBlob(const Hash& id, const Blob* blob);

  /**
   * Put arbitrary data in the store.
   */
  virtual void
  put(KeySpace keySpace, folly::ByteRange key, folly::ByteRange value) = 0;
  void put(KeySpace keySpace, const Hash& id, folly::ByteRange value);

  /*
   * WriteBatch is a helper class for facilitating a bulk store operation.
   *
   * The purpose of this class is to let multiple callers manage independent
   * write batches and flush them to the backing storage when its deemed
   * appropriate.
   *
   * WriteBatch is not safe to mutate from multiple threads concurrently.
   *
   * Typical usage:
   * auto writer = localStore->beginWrite();
   * writer->put(KeySpace::Meta, Key, Value);
   * writer->put(KeySpace::Blob, Key, BlobValue);
   * writer->flush();
   */
  class WriteBatch {
   public:
    /**
     * Store a Tree into the TreeFamily KeySpace.
     *
     * Returns the Hash that can be used to look up the tree later.
     */
    Hash putTree(const Tree* tree);

    /**
     * Store a Blob.
     *
     * Returns a BlobMetadata about the blob, which includes the SHA-1 hash of
     * its contents.
     */
    BlobMetadata putBlob(const Hash& id, const Blob* blob);

    /**
     * Put arbitrary data in the store.
     */
    virtual void
    put(KeySpace keySpace, folly::ByteRange key, folly::ByteRange value) = 0;
    void put(KeySpace keySpace, const Hash& id, folly::ByteRange value);

    /**
     * Put arbitrary data in the store where the value is split across
     * a set of sliced data.
     */
    virtual void put(
        KeySpace keySpace,
        folly::ByteRange key,
        std::vector<folly::ByteRange> valueSlices) = 0;

    /**
     * Flush any pending data to the store.
     */
    virtual void flush() = 0;

    // Forbidden copy construction/assignment; allow only moves
    WriteBatch(const WriteBatch&) = delete;
    WriteBatch(WriteBatch&&) = default;
    WriteBatch& operator=(const WriteBatch&) = delete;
    WriteBatch& operator=(WriteBatch&&) = default;
    virtual ~WriteBatch();
    WriteBatch() = default;

   private:
    friend class LocalStore;
  };

  /**
   * Construct a LocalStoreBatchWrite object with write batch of size bufSize.
   * If bufSize is non-zero the batch will automatically flush each time
   * the accumulated data exceeds bufSize.  Otherwise no implifict flushing
   * will occur.
   * Either way, the caller will typically want to finish up by calling
   * writeBatch->flush() to complete the batch as there is no implicit flush on
   * destruction either.
   */
  virtual std::unique_ptr<WriteBatch> beginWrite(size_t bufSize = 0) = 0;
};
} // namespace eden
} // namespace facebook
