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

#include <initializer_list>
#include <memory>
#include <unordered_map>
#include <vector>
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/store/BackingStore.h"
#include "eden/fs/testharness/StoredObject.h"

namespace facebook {
namespace eden {

class FakeTreeBuilder;
class LocalStore;

/**
 * A BackingStore implementation for test code.
 */
class FakeBackingStore : public BackingStore {
 public:
  struct TreeEntryData;

  explicit FakeBackingStore(std::shared_ptr<LocalStore> localStore);
  virtual ~FakeBackingStore();

  /*
   * BackingStore APIs
   */

  folly::Future<std::unique_ptr<Tree>> getTree(const Hash& id) override;
  folly::Future<std::unique_ptr<Blob>> getBlob(const Hash& id) override;
  folly::Future<std::unique_ptr<Tree>> getTreeForCommit(
      const Hash& commitID) override;

  /**
   * Add a Blob to the backing store
   *
   * If a hash is not explicitly given, one will be computed automatically.
   * (The test code may not use the same hashing scheme as a production
   * mercurial- or git-backed store, but it will be consistent for the
   * duration of the test.)
   */
  StoredBlob* putBlob(folly::StringPiece contents);
  StoredBlob* putBlob(Hash hash, folly::StringPiece contents);

  /**
   * Add a blob to the backing store, or return the StoredBlob already present
   * with this hash.
   *
   * The boolean in the return value is true if a new StoredBlob was created by
   * this call, or false if a StoredBlob already existed with this hash.
   */
  std::pair<StoredBlob*, bool> maybePutBlob(folly::StringPiece contents);
  std::pair<StoredBlob*, bool> maybePutBlob(
      Hash hash,
      folly::StringPiece contents);

  static Blob makeBlob(folly::StringPiece contents);
  static Blob makeBlob(Hash hash, folly::StringPiece contents);

  /**
   * Helper functions for building a tree.
   *
   * Example usage:
   *
   *   store->putTree({
   *       {"test.txt", testBlob, 0644},
   *       {"runme.sh", runmeBlob, 0755},
   *       {"subdir", subdirTree, 0755},
   *   });
   */
  StoredTree* putTree(const std::initializer_list<TreeEntryData>& entries);
  StoredTree* putTree(
      Hash hash,
      const std::initializer_list<TreeEntryData>& entries);
  StoredTree* putTree(std::vector<TreeEntry> entries);
  StoredTree* putTree(Hash hash, std::vector<TreeEntry> entries);

  /**
   * Add a tree to the backing store, or return the StoredTree already present
   * with this hash.
   *
   * The boolean in the return value is true if a new StoredTree was created by
   * this call, or false if a StoredTree already existed with this hash.
   */
  std::pair<StoredTree*, bool> maybePutTree(
      const std::initializer_list<TreeEntryData>& entries);
  std::pair<StoredTree*, bool> maybePutTree(std::vector<TreeEntry> entries);

  /**
   * Add a mapping from a commit ID to a root tree hash.
   */
  StoredHash* putCommit(Hash commitHash, const StoredTree* tree);
  StoredHash* putCommit(Hash commitHash, Hash treeHash);

  StoredHash* putCommit(
      folly::StringPiece commitStr,
      const FakeTreeBuilder& builder);

  /**
   * Look up a StoredTree.
   *
   * Throws an error if the specified hash does not exist.  Never returns null.
   */
  StoredTree* getStoredTree(Hash hash);

  /**
   * Look up a StoredBlob.
   *
   * Throws an error if the specified hash does not exist.  Never returns null.
   */
  StoredBlob* getStoredBlob(Hash hash);

  /**
   * Create a new FakeTreeBuilder that can be used to populate data in this
   * FakeBackingStore.
   */
  FakeTreeBuilder treeBuilder();

 private:
  struct Data {
    std::unordered_map<Hash, std::unique_ptr<StoredTree>> trees;
    std::unordered_map<Hash, std::unique_ptr<StoredBlob>> blobs;
    std::unordered_map<Hash, std::unique_ptr<StoredHash>> commits;
  };

  static std::vector<TreeEntry> buildTreeEntries(
      const std::initializer_list<TreeEntryData>& entryArgs);
  static void sortTreeEntries(std::vector<TreeEntry>& entries);
  static Hash computeTreeHash(const std::vector<TreeEntry>& sortedEntries);
  StoredTree* putTreeImpl(Hash hash, std::vector<TreeEntry>&& sortedEntries);
  std::pair<StoredTree*, bool> maybePutTreeImpl(
      Hash hash,
      std::vector<TreeEntry>&& sortedEntries);

  const std::shared_ptr<LocalStore> localStore_;
  folly::Synchronized<Data> data_;
};

/**
 * A small helper struct for use with FakeBackingStore::putTree()
 *
 * This mainly exists to allow putTree() to be called conveniently with
 * initialier-list arguments.
 */
struct FakeBackingStore::TreeEntryData {
  TreeEntryData(folly::StringPiece name, const Blob& blob, mode_t mode = 0644);
  TreeEntryData(
      folly::StringPiece name,
      const StoredBlob* blob,
      mode_t mode = 0644);
  TreeEntryData(folly::StringPiece name, const Tree& tree, mode_t mode = 0755);
  TreeEntryData(
      folly::StringPiece name,
      const StoredTree* tree,
      mode_t mode = 0755);

  TreeEntry entry;
};
}
} // facebook::eden
