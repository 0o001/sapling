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
#include "eden/fs/inodes/InodePtr.h"
#include "eden/fuse/DirHandle.h"

namespace facebook {
namespace eden {

class TreeInodeDirHandle : public fusell::DirHandle {
 public:
  explicit TreeInodeDirHandle(TreeInodePtr inode);

  folly::Future<fusell::DirList> readdir(fusell::DirList&& list, off_t off)
      override;

  folly::Future<fusell::Dispatcher::Attr> setattr(
      const struct stat& attr,
      int to_set) override;
  folly::Future<folly::Unit> fsyncdir(bool datasync) override;
  folly::Future<fusell::Dispatcher::Attr> getattr() override;

 private:
  TreeInodePtr inode_;
};
}
}
