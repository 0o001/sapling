/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/futures/Promise.h>
#include <utility>
#include <variant>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/ImportPriority.h"
#include "eden/fs/store/hg/HgProxyHash.h"
#include "eden/fs/telemetry/RequestMetricsScope.h"
#include "eden/fs/utils/Bug.h"

namespace facebook {
namespace eden {

/**
 * Represents an Hg import request. This class contains all the necessary
 * information needed to fulfill the request as well as a promise that will be
 * resolved after the requested data is imported.
 */
class HgImportRequest {
 public:
  struct BlobImport {
    using Response = std::unique_ptr<Blob>;

    Hash hash;
    HgProxyHash proxyHash;
  };

  struct TreeImport {
    using Response = std::unique_ptr<Tree>;

    Hash hash;
    HgProxyHash proxyHash;
    // we normally want to prefetch metadata, there are only a few cases where
    // we do not want to
    bool prefetchMetadata = true;
  };

  struct Prefetch {
    using Response = folly::Unit;

    std::vector<HgProxyHash> proxyHashes;
  };

  static std::pair<HgImportRequest, folly::SemiFuture<std::unique_ptr<Blob>>>
  makeBlobImportRequest(
      Hash hash,
      HgProxyHash proxyHash,
      ImportPriority priority,
      std::unique_ptr<RequestMetricsScope> metricsScope);

  static std::pair<HgImportRequest, folly::SemiFuture<std::unique_ptr<Tree>>>
  makeTreeImportRequest(
      Hash hash,
      HgProxyHash proxyHash,
      ImportPriority priority,
      std::unique_ptr<RequestMetricsScope> metricsScope,
      bool prefetchMetadata);

  static std::pair<HgImportRequest, folly::SemiFuture<folly::Unit>>
  makePrefetchRequest(
      std::vector<HgProxyHash> hashes,
      ImportPriority priority,
      std::unique_ptr<RequestMetricsScope> metricsScope);

  template <typename RequestType>
  HgImportRequest(
      RequestType request,
      ImportPriority priority,
      folly::Promise<typename RequestType::Response>&& promise)
      : request_(std::move(request)),
        priority_(priority),
        promise_(std::move(promise)) {}

  ~HgImportRequest() = default;

  HgImportRequest(HgImportRequest&&) = default;
  HgImportRequest& operator=(HgImportRequest&&) = default;

  template <typename T>
  const T* getRequest() noexcept {
    return std::get_if<T>(&request_);
  }

  template <typename T>
  bool isType() const noexcept {
    return std::holds_alternative<T>(request_);
  }

  size_t getType() const noexcept {
    return request_.index();
  }

  template <typename T>
  folly::Promise<T>* getPromise() {
    auto promise = std::get_if<folly::Promise<T>>(&promise_); // Promise<T>

    if (!promise) {
      EDEN_BUG() << "invalid promise type";
    }
    return promise;
  }

 private:
  HgImportRequest(const HgImportRequest&) = delete;
  HgImportRequest& operator=(const HgImportRequest&) = delete;

  using Request = std::variant<BlobImport, TreeImport, Prefetch>;
  using Response = std::variant<
      folly::Promise<std::unique_ptr<Blob>>,
      folly::Promise<std::unique_ptr<Tree>>,
      folly::Promise<folly::Unit>>;

  Request request_;
  ImportPriority priority_;
  Response promise_;

  friend bool operator<(
      const HgImportRequest& lhs,
      const HgImportRequest& rhs) {
    return lhs.priority_ < rhs.priority_;
  }
};

} // namespace eden
} // namespace facebook
