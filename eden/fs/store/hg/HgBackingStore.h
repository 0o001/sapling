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

#include "eden/fs/store/BackingStore.h"
#include "eden/fs/store/hg/HgImporter.h"
#include "eden/fs/utils/PathFuncs.h"

#include <folly/Executor.h>
#include <folly/Range.h>
#include <folly/Synchronized.h>

namespace facebook {
namespace eden {

class LocalStore;

/**
 * A BackingStore implementation that loads data out of a mercurial repository.
 */
class HgBackingStore : public BackingStore {
 public:
  /**
   * Create a new HgBackingStore.
   *
   * The LocalStore object is owned by the EdenServer (which also owns this
   * HgBackingStore object).  It is guaranteed to be valid for the lifetime of
   * the HgBackingStore object.
   */
  HgBackingStore(
      AbsolutePathPiece repository,
      LocalStore* localStore,
      folly::Executor* serverThreadPool);
  ~HgBackingStore() override;

  folly::Future<std::unique_ptr<Tree>> getTree(const Hash& id) override;
  folly::Future<std::unique_ptr<Blob>> getBlob(const Hash& id) override;
  folly::Future<std::unique_ptr<Tree>> getTreeForCommit(
      const Hash& commitID) override;

 private:
  // Forbidden copy constructor and assignment operator
  HgBackingStore(HgBackingStore const&) = delete;
  HgBackingStore& operator=(HgBackingStore const&) = delete;

  std::unique_ptr<Tree> getTreeForCommitImpl(const Hash& commitID);

  LocalStore* localStore_{nullptr};
  // A set of threads owning HgImporter instances
  std::unique_ptr<folly::Executor> importThreadPool_;
  // The main server thread pool; we push the Futures back into
  // this pool to run their completion code to avoid clogging
  // the importer pool.
  folly::Executor* serverThreadPool_;
};
} // namespace eden
} // namespace facebook
