/*
 *  Copyright (c) 2018-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/store/BlobAccess.h"
#include <folly/MapUtil.h>
#include "eden/fs/model/Blob.h"
#include "eden/fs/store/BlobCache.h"
#include "eden/fs/store/IObjectStore.h"

namespace facebook {
namespace eden {

BlobAccess::BlobAccess(
    std::shared_ptr<IObjectStore> objectStore,
    std::shared_ptr<BlobCache> blobCache)
    : objectStore_{std::move(objectStore)}, blobCache_{std::move(blobCache)} {}

BlobAccess::~BlobAccess() {}

folly::Future<BlobCache::GetResult> BlobAccess::getBlob(
    const Hash& hash,
    BlobCache::Interest interest) {
  auto result = blobCache_->get(hash, interest);
  if (result.blob) {
    return std::move(result);
  }

  return objectStore_->getBlob(hash).thenValue(
      [blobCache = blobCache_, interest](std::shared_ptr<const Blob> blob) {
        auto interestHandle = blobCache->insert(blob, interest);
        return BlobCache::GetResult{std::move(blob), std::move(interestHandle)};
      });
}

} // namespace eden
} // namespace facebook
