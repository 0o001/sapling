/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "InodeError.h"

#include <folly/String.h>
#include "eden/fs/inodes/TreeInode.h"

namespace facebook {
namespace eden {

InodeError::InodeError(int errnum, TreeInodePtr inode, PathComponentPiece child)
    : std::system_error(errnum, std::system_category()),
      inode_(std::move(inode)),
      child_(PathComponent{child}) {}

InodeError::InodeError(
    int errnum,
    TreeInodePtr inode,
    PathComponentPiece child,
    std::string&& message)
    : std::system_error(errnum, std::system_category()),
      inode_(std::move(inode)),
      child_(PathComponent{child}),
      message_(std::move(message)) {}

const char* InodeError::what() const noexcept {
  try {
    auto msg = fullMessage_.wlock();
    if (msg->empty()) {
      *msg = computeMessage();
    }

    return msg->c_str();
  } catch (...) {
    // Fallback value if anything goes wrong building the real message
    return "<InodeError>";
  }
}

std::string InodeError::computeMessage() const {
  std::string path;
  if (child_.hasValue()) {
    if (inode_->getNodeId() == FUSE_ROOT_ID) {
      path = child_.value().stringPiece().str();
    } else {
      path = inode_->getLogPath() + "/";
      auto childName = child_.value().stringPiece();
      path.append(childName.begin(), childName.end());
    }
  } else {
    path = inode_->getLogPath();
  }

  if (message_.empty()) {
    return path + ": " + folly::errnoStr(errnum()).toStdString();
  }
  return path + ": " + message_ + ": " +
      folly::errnoStr(errnum()).toStdString();
}
}
}
