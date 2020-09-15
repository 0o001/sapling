/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once
#include <iostream>
#include <string>
#include "eden/fs/service/EdenError.h"
#include "eden/fs/utils/PathFuncs.h"

// This is a stub to compile eden/service on Window.
struct fuse_init_out {
  uint32_t major;
  uint32_t minor;
};

namespace facebook {
namespace eden {

class SerializedInodeMap {
  int stub;
};

struct SerializedFileHandleMap {
  int stub;
};

using uid_t = int;
using gid_t = int;

struct FuseChannelData {
  //  folly::File fd;
  int fd;
  fuse_init_out connInfo;
};

static int unlink(const char* path) {
  // Ideally unlink should be a part of folly portability layer but there is a
  // deprecated definition of unlink in stdio which will make it ambiguous and
  // break the build for other softwares using folly on Windows.
  return _unlink(path);
}

#define NOT_IMPLEMENTED()                             \
  do {                                                \
    throw newEdenError(                               \
        EdenErrorType::GENERIC_ERROR,                 \
        " +++++  NOT IMPLEMENTED +++++++ Function: ", \
        __FUNCTION__,                                 \
        " Line: ",                                    \
        __LINE__);                                    \
  } while (true)

} // namespace eden
} // namespace facebook
