/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/Range.h>
#include <folly/futures/Future.h>

#include "eden/fs/store/IObjectStore.h"
#include "eden/fs/utils/PathFuncs.h"

namespace folly {
template <typename T>
class Future;
} // namespace folly

namespace apache {
namespace thrift {
class ResponseChannelRequest;
}
} // namespace apache

namespace facebook {
namespace eden {

class DiffCallback;
class GitIgnoreStack;
class ObjectFetchContext;
class ObjectStore;
class UserInfo;
class TopLevelIgnores;
class EdenMount;

/**
 * A helper class to store parameters for a TreeInode::diff() operation.
 *
 * These parameters remain fixed across all subdirectories being diffed.
 * Primarily intent is to compound related diff attributes.
 *
 * The DiffContext must be alive for the duration of the async operation it is
 * used in.
 */
class DiffContext {
 public:
  using LoadFileFunction = std::function<
      folly::Future<std::string>(ObjectFetchContext&, RelativePathPiece)>;

  DiffContext(
      DiffCallback* cb,
      bool listIgnored,
      const ObjectStore* os,
      std::unique_ptr<TopLevelIgnores> topLevelIgnores,
      LoadFileFunction loadFileContentsFromPath,
      apache::thrift::ResponseChannelRequest* FOLLY_NULLABLE request = nullptr);
  DiffContext(DiffCallback* cb, const ObjectStore* os);

  DiffContext(const DiffContext&) = delete;
  DiffContext& operator=(const DiffContext&) = delete;
  DiffContext(DiffContext&&) = delete;
  DiffContext& operator=(DiffContext&&) = delete;
  ~DiffContext();

  DiffCallback* const callback;
  const ObjectStore* const store;
  /**
   * If listIgnored is true information about ignored files will be reported.
   * If listIgnored is false then ignoredFile() will never be called on the
   * callback.  The diff operation may be faster with listIgnored=false, since
   * it can completely omit processing ignored subdirectories.
   */
  bool const listIgnored;

  const GitIgnoreStack* getToplevelIgnore() const;
  bool isCancelled() const;
  LoadFileFunction getLoadFileContentsFromPath() const;
  ObjectFetchContext& getFetchContext() {
    return fetchContext_;
  }

 private:
  std::unique_ptr<TopLevelIgnores> topLevelIgnores_;
  const LoadFileFunction loadFileContentsFromPath_;
  apache::thrift::ResponseChannelRequest* const FOLLY_NULLABLE request_;
  ObjectFetchContext fetchContext_;
};
} // namespace eden
} // namespace facebook
