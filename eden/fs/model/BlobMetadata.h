/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <cstdint>
#include "eden/fs/model/BlobMetadataFwd.h"
#include "eden/fs/model/Hash.h"

namespace facebook::eden {

/**
 * A small struct containing both the size and the SHA-1 hash of
 * a Blob's contents.
 */
class BlobMetadata {
 public:
  BlobMetadata(Hash20 contentsHash, uint64_t fileLength)
      : sha1(contentsHash), size(fileLength) {}

  Hash20 sha1;
  uint64_t size;
};

} // namespace facebook::eden
