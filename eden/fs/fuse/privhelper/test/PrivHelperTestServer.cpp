/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "PrivHelperTestServer.h"

#include <boost/filesystem.hpp>
#include <folly/File.h>
#include <folly/FileUtil.h>
#include <system_error>
#include "eden/fs/utils/SystemError.h"

using folly::File;
using folly::StringPiece;
using std::string;

namespace facebook {
namespace eden {

PrivHelperTestServer::PrivHelperTestServer() {}

// FUSE mounts.

File PrivHelperTestServer::fuseMount(const char* mountPath) {
  // Create a single file named "mounted" and write "mounted" into it.
  auto pathToNewFile = getPathToMountMarker(mountPath);
  File f(pathToNewFile, O_RDWR | O_CREAT | O_TRUNC);
  StringPiece data{"mounted"};
  folly::writeFull(f.fd(), data.data(), data.size());
  return f;
}

void PrivHelperTestServer::fuseUnmount(const char* mountPath) {
  // Replace the file contents with "unmounted".
  folly::writeFile(
      StringPiece{"unmounted"}, getPathToMountMarker(mountPath).c_str());
}

bool PrivHelperTestServer::isMounted(folly::StringPiece mountPath) const {
  return checkIfMarkerFileHasContents(
      getPathToMountMarker(mountPath), "mounted");
}

string PrivHelperTestServer::getPathToMountMarker(StringPiece mountPath) const {
  return mountPath.str() + "/mounted";
}

// Bind mounts.

void PrivHelperTestServer::bindMount(
    const char* /*clientPath*/,
    const char* mountPath) {
  // Create a single file named "bind-mounted" and write "bind-mounted" into it.

  // Normally, the caller to the PrivHelper (in practice, EdenServer) is
  // responsible for creating the directory before requesting the bind mount.
  boost::filesystem::create_directories(mountPath);

  auto fileInMountPath = getPathToBindMountMarker(mountPath);
  folly::writeFile(StringPiece{"bind-mounted"}, fileInMountPath.c_str());
}

void PrivHelperTestServer::bindUnmount(const char* mountPath) {
  // Replace the file contents with "bind-unmounted".
  folly::writeFile(
      StringPiece{"bind-unmounted"},
      getPathToBindMountMarker(mountPath).c_str());
}

bool PrivHelperTestServer::isBindMounted(folly::StringPiece mountPath) const {
  return checkIfMarkerFileHasContents(
      getPathToBindMountMarker(mountPath), "bind-mounted");
}

string PrivHelperTestServer::getPathToBindMountMarker(
    StringPiece mountPath) const {
  return mountPath.str() + "/bind-mounted";
}

// General helpers.

bool PrivHelperTestServer::checkIfMarkerFileHasContents(
    const string pathToMarkerFile,
    const string contents) const {
  try {
    string data;
    folly::readFile(pathToMarkerFile.c_str(), data, 256);
    return data == contents;
  } catch (const std::system_error& ex) {
    if (isEnoent(ex)) {
      // Looks like this was never mounted
      return false;
    }
    throw;
  }
}

} // namespace eden
} // namespace facebook
