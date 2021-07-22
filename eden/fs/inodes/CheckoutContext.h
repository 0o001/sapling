/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <vector>

#include <folly/Range.h>
#include <folly/Synchronized.h>
#include <folly/stop_watch.h>

#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/InodePtrFwd.h"
#include "eden/fs/service/gen-cpp2/eden_types.h"
#include "eden/fs/store/StatsFetchContext.h"
#include "eden/fs/utils/PathFuncs.h"

namespace folly {
class exception_wrapper;
template <typename T>
class Future;
struct Unit;
} // namespace folly

namespace facebook {
namespace eden {

class CheckoutConflict;
class TreeInode;
class Tree;

/**
 * CheckoutContext maintains state during a checkout operation.
 */
class CheckoutContext {
 public:
  CheckoutContext(
      EdenMount* mount,
      folly::Synchronized<RootId>::LockedPtr&& parentLock,
      CheckoutMode checkoutMode,
      std::optional<pid_t> clientPid,
      folly::StringPiece thriftMethodName);

  CheckoutContext(
      EdenMount* mount,
      CheckoutMode checkoutMode,
      std::optional<pid_t> clientPid,
      folly::StringPiece thriftMethodName);

  ~CheckoutContext();

  /**
   * Returns true if the checkout operation should do a dry run, looking for
   * conflicts without actually updating the inode contents. If it returns
   * false, it should actually update the inodes as part of the checkout.
   */
  bool isDryRun() const {
    // TODO: make this configurable on checkout start
    return checkoutMode_ == CheckoutMode::DRY_RUN;
  }

  /**
   * Returns true if this checkout operation should force the new inode
   * contents to look like the data in the Tree being checked out, even if
   * there are conflicts.
   *
   * This will cause the checkout to always update files with conflicts to the
   * new contents, rather than just reporting and skipping files with
   * conflicts.
   *
   * forceUpdate() can only return true when isDryRun() is false.
   */
  bool forceUpdate() const {
    return checkoutMode_ == CheckoutMode::FORCE;
  }

  /**
   * Start the checkout operation.
   */
  void start(RenameLock&& renameLock);

  /**
   * Complete the checkout operation
   *
   * Returns the list of conflicts and errors that were encountered during the
   * operation.
   */
  folly::Future<std::vector<CheckoutConflict>> finish(RootId newSnapshot);

  void addConflict(ConflictType type, RelativePathPiece path);
  void
  addConflict(ConflictType type, TreeInode* parent, PathComponentPiece name);
  void addConflict(ConflictType type, InodeBase* inode);

  void addError(
      TreeInode* parent,
      PathComponentPiece name,
      const folly::exception_wrapper& ew);

  /**
   * Get a reference to the rename lock.
   *
   * This is mostly used for APIs that require proof that we are currently
   * holding the lock.
   */
  const RenameLock& renameLock() const {
    return renameLock_;
  }

  /**
   * Return the fetch context associated with this checkout context.
   */
  StatsFetchContext& getFetchContext() {
    return fetchContext_;
  }

 private:
  CheckoutMode checkoutMode_;
  EdenMount* const mount_;
  folly::Synchronized<RootId>::LockedPtr parentLock_;
  RenameLock renameLock_;
  StatsFetchContext fetchContext_;

  // The checkout processing may occur across many threads,
  // if some data load operations complete asynchronously on other threads.
  // Therefore access to the conflicts list must be synchronized.
  folly::Synchronized<std::vector<CheckoutConflict>> conflicts_;
};
} // namespace eden
} // namespace facebook
