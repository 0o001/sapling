/*
 *  Copyright (c) 2019-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/store/mononoke/MononokeCurlBackingStore.h"

#include <folly/Executor.h>
#include <folly/ThreadLocal.h>
#include <folly/executors/CPUThreadPoolExecutor.h>
#include <folly/executors/task_queue/UnboundedBlockingQueue.h>
#include <folly/executors/thread_factory/NamedThreadFactory.h>
#include <folly/executors/thread_factory/ThreadFactory.h>
#include <folly/json.h>
#include <folly/logging/xlog.h>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/mononoke/CurlHttpClient.h"
#include "eden/fs/store/mononoke/MononokeAPIUtils.h"
#include "eden/fs/utils/PathFuncs.h"
#include "eden/fs/utils/ServiceAddress.h"

DEFINE_int32(
    mononoke_curl_threads,
    8,
    "number of curl client threads for Mononoke");

namespace facebook {
namespace eden {

namespace {

static folly::ThreadLocalPtr<CurlHttpClient> threadCurlClient;

CurlHttpClient& getCurlHttpClient() {
  if (!threadCurlClient) {
    throw std::logic_error(
        "Attempting to use curl client in a non-curl client thread "
        "or failed to resolve service address");
  }
  return *threadCurlClient;
}

class MononokeCurlThreadFactory : public folly::ThreadFactory {
 public:
  MononokeCurlThreadFactory(
      std::unique_ptr<ServiceAddress> service,
      AbsolutePath certificate,
      std::chrono::milliseconds timeout)
      : delegate_("CurlClient"),
        service_(std::move(service)),
        certificate_(certificate),
        timeout_(timeout) {}

  std::thread newThread(folly::Func&& func) override {
    return delegate_.newThread([this, func = std::move(func)]() mutable {
      try {
        auto address = service_->getSocketAddressBlocking();
        if (address) {
          threadCurlClient.reset(
              new CurlHttpClient(address->first, certificate_, timeout_));
          func();
        } else {
          XLOG(WARN) << "failed to resolve address for Mononoke API Server";
        }
      } catch (const std::exception& ex) {
        XLOG(WARN)
            << "failed to resolve address for Mononoke API Server, reason: "
            << ex.what();
      }
    });
  }

 private:
  folly::NamedThreadFactory delegate_;
  std::unique_ptr<ServiceAddress> service_;
  AbsolutePath certificate_;
  const std::chrono::milliseconds timeout_;
}; // namespace
} // namespace

MononokeCurlBackingStore::MononokeCurlBackingStore(
    std::unique_ptr<ServiceAddress> service,
    AbsolutePath certificate,
    std::string repo,
    std::chrono::milliseconds timeout,
    std::shared_ptr<folly::Executor> executor)
    : repo_(std::move(repo)),
      clientThreadPool_(std::make_unique<folly::CPUThreadPoolExecutor>(
          FLAGS_mononoke_curl_threads,
          std::make_unique<folly::UnboundedBlockingQueue<
              folly::CPUThreadPoolExecutor::CPUTask>>(),
          std::make_shared<MononokeCurlThreadFactory>(
              std::move(service),
              certificate,
              timeout))),
      serverExecutor_(std::move(executor)) {}

folly::Future<std::unique_ptr<Tree>> MononokeCurlBackingStore::getTree(
    const Hash& id) {
  return folly::via(
             clientThreadPool_.get(),
             [this, id] {
               return getCurlHttpClient().get(
                   buildMononokePath("tree", id.toString()));
             })
      .via(serverExecutor_.get())
      .thenValue([id](std::unique_ptr<folly::IOBuf>&& buf) {
        return parseMononokeTree(std::move(buf), id);
      });
}

folly::Future<std::unique_ptr<Blob>> MononokeCurlBackingStore::getBlob(
    const Hash& id) {
  return folly::via(
             clientThreadPool_.get(),
             [this, id] {
               return getCurlHttpClient().get(
                   buildMononokePath("blob", id.toString()));
             })
      .via(serverExecutor_.get())
      .thenValue([id](std::unique_ptr<folly::IOBuf>&& buf) {
        return std::make_unique<Blob>(id, *buf);
      });
}

folly::Future<std::unique_ptr<Tree>> MononokeCurlBackingStore::getTreeForCommit(
    const Hash& commitID) {
  return folly::via(
             clientThreadPool_.get(),
             [this, commitID] {
               return getCurlHttpClient().get(
                   buildMononokePath("manifest", commitID.toString()));
             })
      .via(serverExecutor_.get())
      .thenValue([&](std::unique_ptr<folly::IOBuf>&& buf) {
        auto hash = Hash(
            folly::parseJson(buf->moveToFbString()).at("manifest").asString());
        return getTree(hash);
      });
}

std::string MononokeCurlBackingStore::buildMononokePath(
    folly::StringPiece action,
    folly::StringPiece args) {
  return folly::to<std::string>("/", repo_, "/", action, "/", args);
}
} // namespace eden
} // namespace facebook
