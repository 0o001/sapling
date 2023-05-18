/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "SerializedBlobMetadata.h"

#include <folly/Range.h>
#include <folly/lang/Bits.h>

#include "eden/fs/model/BlobMetadata.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/utils/Throw.h"

namespace facebook::eden {

SerializedBlobMetadata::SerializedBlobMetadata(const BlobMetadata& metadata) {
  serialize(metadata.sha1, metadata.size);
}

SerializedBlobMetadata::SerializedBlobMetadata(
    const Hash20& contentsHash,
    uint64_t blobSize) {
  serialize(contentsHash, blobSize);
}

folly::ByteRange SerializedBlobMetadata::slice() const {
  return folly::ByteRange{data_};
}

namespace {
BlobMetadataPtr unslice(folly::ByteRange bytes) {
  uint64_t blobSizeBE;
  memcpy(&blobSizeBE, bytes.data(), sizeof(uint64_t));
  bytes.advance(sizeof(uint64_t));
  auto contentsHash = Hash20{bytes};
  return std::make_shared<BlobMetadataPtr::element_type>(
      contentsHash, folly::Endian::big(blobSizeBE));
}
} // namespace

BlobMetadataPtr SerializedBlobMetadata::parse(
    ObjectId blobID,
    const StoreResult& result) {
  auto bytes = result.bytes();
  if (bytes.size() != SIZE) {
    throwf<std::invalid_argument>(
        "Blob metadata for {} had unexpected size {}. Could not deserialize.",
        blobID,
        bytes.size());
  }

  return unslice(bytes);
}

void SerializedBlobMetadata::serialize(
    const Hash20& contentsHash,
    uint64_t blobSize) {
  uint64_t blobSizeBE = folly::Endian::big(blobSize);
  memcpy(data_.data(), &blobSizeBE, sizeof(uint64_t));
  memcpy(
      data_.data() + sizeof(uint64_t),
      contentsHash.getBytes().data(),
      Hash20::RAW_SIZE);
}

} // namespace facebook::eden
