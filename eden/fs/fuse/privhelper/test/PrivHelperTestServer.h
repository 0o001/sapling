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

#include "eden/fs/fuse/privhelper/PrivHelperServer.h"

#include <folly/Range.h>

namespace facebook {
namespace eden {

/*
 * A subclass of PrivHelperServer that doesn't actually perform
 * real mounts and unmounts.  This lets us use it in unit tests
 * when we are running without root privileges.
 */
class PrivHelperTestServer : public PrivHelperServer {
 public:
  PrivHelperTestServer();

  /*
   * Check if the given mount point is mounted.
   *
   * This can be called from any process.  (It is generally called from the
   * main process during unit tests, and not from the privhelper process.)
   */
  bool isMounted(folly::StringPiece mountPath) const;

  /**
   * Check if the given path is bind mounted.
   */
  bool isBindMounted(folly::StringPiece mountPath) const;

 private:
  folly::File fuseMount(const char* mountPath) override;
  void fuseUnmount(const char* mountPath) override;
  std::string getPathToMountMarker(folly::StringPiece mountPath) const;

  void bindMount(const char* clientPath, const char* mountPath) override;
  void bindUnmount(const char* mountPath) override;
  std::string getPathToBindMountMarker(folly::StringPiece mountPath) const;

  /** @return true if the marker file exists with the specified contents. */
  bool checkIfMarkerFileHasContents(
      const std::string pathToMarkerFile,
      const std::string contents) const;
};

} // namespace eden
} // namespace facebook
