/*
 *  Copyright (c) 2017-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "MononokeBackingStore.h"

#include <eden/fs/model/Blob.h>
#include <eden/fs/model/Hash.h>
#include <eden/fs/model/Tree.h>
#include <folly/futures/Future.h>
#include <folly/futures/Promise.h>
#include <folly/io/async/EventBase.h>
#include <folly/io/async/EventBaseManager.h>
#include <folly/json.h>
#include <proxygen/lib/http/HTTPConnector.h>
#include <proxygen/lib/http/session/HTTPUpstreamSession.h>
#include <proxygen/lib/utils/URL.h>

using folly::Future;
using folly::IOBuf;
using folly::make_exception_wrapper;
using folly::makeFuture;
using proxygen::ErrorCode;
using proxygen::HTTPException;
using proxygen::HTTPHeaders;
using proxygen::HTTPMessage;
using proxygen::HTTPTransaction;
using proxygen::UpgradeProtocol;
using proxygen::URL;

namespace facebook {
namespace eden {
namespace {

using IOBufPromise = folly::Promise<std::unique_ptr<folly::IOBuf>>;

// Callback that processes response for getblob request.
// Note: because this callback deletes itself, it must be allocated on the heap!
class MononokeCallback : public proxygen::HTTPConnector::Callback,
                         public proxygen::HTTPTransaction::Handler {
 public:
  MononokeCallback(const proxygen::URL& url, IOBufPromise&& promise)
      : promise_(std::move(promise)), url_(url) {}

  virtual void connectSuccess(proxygen::HTTPUpstreamSession* session) override {
    auto txn = session->newTransaction(this);
    HTTPMessage message;
    message.setMethod(proxygen::HTTPMethod::GET);
    message.setURL(url_.makeRelativeURL());
    txn->sendHeaders(message);
    txn->sendEOM();
    session->closeWhenIdle();
  }

  void connectError(const folly::AsyncSocketException& /* ex */) override {
    // handler won't be used anymore, should be safe to delete
    promise_.setException(
        make_exception_wrapper<std::runtime_error>("connect error"));
    delete this;
  }

  // We don't send anything back, so ignore this callback
  void setTransaction(HTTPTransaction* /* txn */) noexcept override {}

  void detachTransaction() noexcept override {
    if (error_) {
      promise_.setException(error_);
    } else {
      if (body_ == nullptr) {
        // Make sure we return empty buffer and not nullptr to the caller.
        // It can happen if blob is empty.
        body_ = folly::IOBuf::create(0);
      }
      if (isSuccessfulStatusCode()) {
        promise_.setValue(std::move(body_));
      } else {
        auto error_msg = folly::to<std::string>(
            "request failed: ",
            status_code_,
            ", ",
            body_ ? body_->moveToFbString() : "");
        promise_.setException(
            make_exception_wrapper<std::runtime_error>(error_msg));
      }
    }

    /*
    From proxygen source code comments (HTTPTransaction.h):
        The Handler deletes itself at some point after the Transaction
        has detached from it.

    After detachTransaction() call Handler won't be used and should be safe to
    delete.
    */
    delete this;
  }

  void onHeadersComplete(std::unique_ptr<HTTPMessage> msg) noexcept override {
    status_code_ = msg->getStatusCode();
  }

  void onBody(std::unique_ptr<folly::IOBuf> chain) noexcept override {
    if (!body_) {
      body_.swap(chain);
      last_ = body_->next();
    } else {
      last_->appendChain(std::move(chain));
      last_ = last_->next();
    }
  }

  void onChunkHeader(size_t /* length */) noexcept override {}

  void onChunkComplete() noexcept override {}

  void onTrailers(
      std::unique_ptr<HTTPHeaders> /* trailers */) noexcept override {}

  void onEOM() noexcept override {}

  void onUpgrade(UpgradeProtocol /* protocol */) noexcept override {}

  void onError(const HTTPException& error) noexcept override {
    auto exception =
        make_exception_wrapper<std::runtime_error>(error.describe());
    error_.swap(exception);
  }

  void onEgressPaused() noexcept override {}

  void onEgressResumed() noexcept override {}

  void onPushedTransaction(HTTPTransaction* /* txn */) noexcept override {}

  void onGoaway(ErrorCode /* code */) noexcept override {}

 private:
  bool isSuccessfulStatusCode() {
    // 2xx are successful status codes
    return (status_code_ / 100) == 2;
  }

  IOBufPromise promise_;
  proxygen::URL url_;
  Hash hash_;
  std::string repo_;
  uint16_t status_code_{0};
  std::unique_ptr<folly::IOBuf> body_{nullptr};
  // Pointer to the last IOBuf in a chain
  folly::IOBuf* last_{nullptr};
  folly::exception_wrapper error_{nullptr};
};

std::unique_ptr<Tree> convertBufToTree(
    std::unique_ptr<folly::IOBuf>&& buf,
    const Hash& id) {
  auto s = buf->moveToFbString();
  auto parsed = folly::parseJson(s);
  if (!parsed.isArray()) {
    throw std::runtime_error("malformed json: should be array");
  }

  std::vector<TreeEntry> entries;
  for (auto i = parsed.begin(); i != parsed.end(); ++i) {
    auto path_elem = i->at("path").asString();
    auto hash = Hash(i->at("hash").asString());
    auto str_type = i->at("type").asString();
    FileType file_type;
    uint8_t owner_permissions = 0b110;
    if (str_type == "File") {
      file_type = FileType::REGULAR_FILE;
    } else if (str_type == "Tree") {
      file_type = FileType::DIRECTORY;
      owner_permissions = 0b111;
    } else if (str_type == "Executable") {
      file_type = FileType::REGULAR_FILE;
      owner_permissions = 0b111;
    } else if (str_type == "Symlink") {
      file_type = FileType::SYMLINK;
      owner_permissions = 0b111;
    } else {
      throw std::runtime_error("unknown file type");
    }
    entries.push_back(TreeEntry(hash, path_elem, file_type, owner_permissions));
  }
  return std::make_unique<Tree>(std::move(entries), id);
}

} // namespace

MononokeBackingStore::MononokeBackingStore(
    const folly::SocketAddress& sa,
    const std::string& repo,
    const std::chrono::milliseconds& timeout,
    folly::Executor* executor)
    : sa_(sa), repo_(repo), timeout_(timeout), executor_(executor) {}

MononokeBackingStore::~MononokeBackingStore() {}

folly::Future<std::unique_ptr<Tree>> MononokeBackingStore::getTree(
    const Hash& id) {
  URL url(folly::sformat("/{}/treenode/{}/", repo_, id.toString()));

  return folly::via(executor_)
      .then([this, url] { return sendRequest(url); })
      .then([id](std::unique_ptr<folly::IOBuf>&& buf) {
        return convertBufToTree(std::move(buf), id);
      });
}

folly::Future<std::unique_ptr<Blob>> MononokeBackingStore::getBlob(
    const Hash& id) {
  URL url(folly::sformat("/{}/blob/{}/", repo_, id.toString()));
  return folly::via(executor_)
      .then([this, url] { return sendRequest(url); })
      .then([id](std::unique_ptr<folly::IOBuf>&& buf) {
        return std::make_unique<Blob>(id, *buf);
      });
}

folly::Future<std::unique_ptr<Tree>> MononokeBackingStore::getTreeForCommit(
    const Hash& commitID) {
  URL url(folly::sformat(
      "/{}/cs/{}/roottreemanifestid/", repo_, commitID.toString()));
  return folly::via(executor_)
      .then([this, url] { return sendRequest(url); })
      .then([&](std::unique_ptr<folly::IOBuf>&& buf) {
        auto treeId = Hash(buf->moveToFbString());
        return getTree(treeId);
      });
}

Future<std::unique_ptr<IOBuf>> MononokeBackingStore::sendRequest(
    const URL& url) {
  auto eventBase = folly::EventBaseManager::get()->getEventBase();
  IOBufPromise promise;
  auto future = promise.getFuture();
  // MononokeCallback deletes itself - see detachTransaction() method
  MononokeCallback* callback = new MononokeCallback(url, std::move(promise));
  // It is moved into the .then() lambda below and destroyed there
  folly::HHWheelTimer::UniquePtr timer{folly::HHWheelTimer::newTimer(
      eventBase,
      std::chrono::milliseconds(folly::HHWheelTimer::DEFAULT_TICK_INTERVAL),
      folly::AsyncTimeout::InternalEnum::NORMAL,
      timeout_)};

  // It is moved into the .then() lambda below and deleted there
  auto connector =
      std::make_unique<proxygen::HTTPConnector>(callback, timer.get());

  const folly::AsyncSocket::OptionMap opts{{{SOL_SOCKET, SO_REUSEADDR}, 1}};
  connector->connect(eventBase, sa_, timeout_, opts);

  /* capture `connector` to make sure it stays alive for the duration of the
     connection */
  return future.then(
      [connector = std::move(connector), timer = std::move(timer)](
          std::unique_ptr<folly::IOBuf>&& buf) { return std::move(buf); });
}

} // namespace eden
} // namespace facebook
