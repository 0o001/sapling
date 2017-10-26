/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */

#include <boost/regex.hpp>
#include <folly/experimental/TestUtil.h>
#include <folly/experimental/logging/Init.h>
#include <folly/experimental/logging/xlog.h>
#include <folly/test/TestUtils.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <proxygen/httpserver/HTTPServer.h>
#include <proxygen/httpserver/RequestHandler.h>
#include <proxygen/httpserver/ResponseBuilder.h>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/LocalStore.h"
#include "eden/fs/store/mononoke/MononokeBackingStore.h"

using namespace facebook::eden;
using namespace proxygen;
using folly::SocketAddress;

using BlobContents = std::map<std::string, std::string>;

class Handler : public proxygen::RequestHandler {
 public:
  explicit Handler(const BlobContents& blobs)
      : regex_(
            "^(/repo/blob/(.*)/|"
            "/repo/treenode/(.*)/|"
            "/repo/cs/(.*)/roottreemanifestid/)$"),
        path_(),
        blobs_(blobs) {}

  ~Handler() {}

  void onRequest(
      std::unique_ptr<proxygen::HTTPMessage> headers) noexcept override {
    path_ = headers->getPath();
  }

  void onBody(std::unique_ptr<folly::IOBuf> /* body */) noexcept override {}

  void onEOM() noexcept override {
    boost::cmatch m;
    auto match = boost::regex_match(path_.c_str(), m, regex_);
    if (match) {
      std::string content;
      if (blobs_.find(m[2]) != blobs_.end()) {
        content = blobs_[m[2]];
      } else if (blobs_.find(m[3]) != blobs_.end()) {
        content = blobs_[m[3]];
      } else if (blobs_.find(m[4]) != blobs_.end()) {
        content = blobs_[m[4]];
      } else {
        ResponseBuilder(downstream_)
            .status(404, "not found")
            .body("cannot find content")
            .sendWithEOM();
      }
      // Split the data in two to make sure that client's onBody() callback
      // works fine
      ResponseBuilder(downstream_).status(200, "OK").send();

      for (auto c : content) {
        // Send characters one by one to make sure client's onBody() methods
        // works correctly.
        ResponseBuilder(downstream_).body(c).send();
      }
      ResponseBuilder(downstream_).sendWithEOM();
    } else {
      ResponseBuilder(downstream_)
          .status(404, "not found")
          .body("malformed url")
          .sendWithEOM();
    }
  }

  void onUpgrade(proxygen::UpgradeProtocol /* proto */) noexcept override {}

  void requestComplete() noexcept override {
    delete this;
  }

  void onError(proxygen::ProxygenError /* err */) noexcept override {}

 private:
  boost::regex regex_;
  std::string path_;
  BlobContents blobs_;
};

class HandlerFactory : public RequestHandlerFactory {
 public:
  explicit HandlerFactory(const BlobContents& blobs) : blobs_(blobs) {}

  void onServerStart(folly::EventBase* /*evb*/) noexcept override {}

  void onServerStop() noexcept override {}

  RequestHandler* onRequest(RequestHandler*, HTTPMessage*) noexcept override {
    return new Handler(blobs_);
  }

 private:
  BlobContents blobs_;
};

class MononokeBackingStoreTest : public ::testing::Test {
 protected:
  std::unique_ptr<HTTPServer> createServer() {
    std::string ip("localhost");
    auto port = 0; // choose any free port
    std::vector<HTTPServer::IPConfig> IPs = {
        {SocketAddress(ip, port, true), HTTPServer::Protocol::HTTP},
    };

    auto blobs = getBlobs();
    HTTPServerOptions options;
    options.threads = 1;
    options.handlerFactories =
        RequestHandlerChain().addThen<HandlerFactory>(blobs).build();
    auto server = folly::make_unique<HTTPServer>(std::move(options));
    server->bind(IPs);
    return server;
  }

  BlobContents getBlobs() {
    BlobContents blobs = {
        std::make_pair(kZeroHash.toString(), "fileblob"),
        std::make_pair(emptyhash.toString(), ""),
        std::make_pair(malformedhash.toString(), "{"),
        std::make_pair(
            treehash.toString(),
            R"([{"hash": "b80de5d138758541c5f05265ad144ab9fa86d1db", "path": "a", "size": 0, "type": "File"},
                {"hash": "b8e02f6433738021a065f94175c7cd23db5f05be", "path": "b", "size": 2, "type": "File"},
                {"hash": "3333333333333333333333333333333333333333", "path": "dir", "size": 2, "type": "Tree"},
                {"hash": "4444444444444444444444444444444444444444", "path": "exec", "size": 2, "type": "Executable"},
                {"hash": "5555555555555555555555555555555555555555", "path": "link", "size": 2, "type": "Symlink"}
            ])"),
        std::make_pair(commithash.toString(), treehash.toString())};
    return blobs;
  }

  Hash emptyhash{"1111111111111111111111111111111111111111"};
  Hash treehash{"2222222222222222222222222222222222222222"};
  Hash commithash{"3333333333333333333333333333333333333333"};
  Hash malformedhash{"9999999999999999999999999999999999999999"};
};

TEST_F(MononokeBackingStoreTest, testGetBlob) {
  auto server = createServer();
  auto blobs = getBlobs();
  std::thread t([&]() {
    server->start([&server, &blobs]() {
      MononokeBackingStore store(
          server->addresses()[0].address,
          "repo",
          std::chrono::milliseconds(400));
      auto blob = store.getBlob(kZeroHash).get();
      auto buf = blob->getContents();
      EXPECT_EQ(blobs[kZeroHash.toString()], buf.moveToFbString());
      server->stop();
    });
  });

  t.join();
}

TEST_F(MononokeBackingStoreTest, testConnectFailed) {
  auto server = createServer();
  auto blobs = getBlobs();

  auto port = server->addresses()[0].address.getPort();
  auto sa = SocketAddress("localhost", port, true);
  MononokeBackingStore store(sa, "repo", std::chrono::milliseconds(300));
  try {
    store.getBlob(kZeroHash).get();
    // Request should fail
    EXPECT_TRUE(false);
  } catch (const std::runtime_error&) {
  }
}

TEST_F(MononokeBackingStoreTest, testEmptyBuffer) {
  auto server = createServer();
  auto blobs = getBlobs();
  auto emptyhash = this->emptyhash;
  std::thread t([&]() {
    server->start([&server, &blobs, emptyhash]() {
      MononokeBackingStore store(
          server->addresses()[0].address,
          "repo",
          std::chrono::milliseconds(300));
      auto blob = store.getBlob(emptyhash).get();
      auto buf = blob->getContents();
      EXPECT_EQ(blobs[emptyhash.toString()], buf.moveToFbString());
      server->stop();
    });
  });

  t.join();
}

TEST_F(MononokeBackingStoreTest, testGetTree) {
  auto server = createServer();
  auto blobs = getBlobs();
  auto treehash = this->treehash;
  std::thread t([&]() {
    server->start([&server, &blobs, treehash]() {
      MononokeBackingStore store(
          server->addresses()[0].address,
          "repo",
          std::chrono::milliseconds(300));
      auto tree = store.getTree(treehash).get();
      auto tree_entries = tree->getTreeEntries();

      std::vector<TreeEntry> expected_entries{
          TreeEntry(
              Hash("b80de5d138758541c5f05265ad144ab9fa86d1db"),
              "a",
              FileType::REGULAR_FILE,
              0b110),
          TreeEntry(
              Hash("b8e02f6433738021a065f94175c7cd23db5f05be"),
              "b",
              FileType::REGULAR_FILE,
              0b110),
          TreeEntry(
              Hash("3333333333333333333333333333333333333333"),
              "dir",
              FileType::DIRECTORY,
              0b111),
          TreeEntry(
              Hash("4444444444444444444444444444444444444444"),
              "exec",
              FileType::REGULAR_FILE,
              0b111),
          TreeEntry(
              Hash("5555555555555555555555555555555555555555"),
              "link",
              FileType::SYMLINK,
              0b111),
      };

      Tree expected_tree(std::move(expected_entries), treehash);
      EXPECT_TRUE(expected_tree == *tree);
      server->stop();
    });
  });

  t.join();
}

TEST_F(MononokeBackingStoreTest, testMalformedGetTree) {
  auto server = createServer();
  auto blobs = getBlobs();
  auto treehash = this->malformedhash;
  std::thread t([&]() {
    server->start([&server, &blobs, treehash]() {
      MononokeBackingStore store(
          server->addresses()[0].address,
          "repo",
          std::chrono::milliseconds(300));
      EXPECT_THROW(store.getTree(treehash).get(), std::exception);
      server->stop();
    });
  });

  t.join();
}

TEST_F(MononokeBackingStoreTest, testGetTreeForCommit) {
  auto server = createServer();
  auto blobs = getBlobs();
  auto commithash = this->commithash;
  auto treehash = this->treehash;
  std::thread t([&]() {
    server->start([&server, commithash, treehash]() {
      MononokeBackingStore store(
          server->addresses()[0].address,
          "repo",
          std::chrono::milliseconds(300));
      auto tree = store.getTreeForCommit(commithash).get();
      auto tree_entries = tree->getTreeEntries();

      std::vector<TreeEntry> expected_entries{
          TreeEntry(
              Hash("b80de5d138758541c5f05265ad144ab9fa86d1db"),
              "a",
              FileType::REGULAR_FILE,
              0b110),
          TreeEntry(
              Hash("b8e02f6433738021a065f94175c7cd23db5f05be"),
              "b",
              FileType::REGULAR_FILE,
              0b110),
          TreeEntry(
              Hash("3333333333333333333333333333333333333333"),
              "dir",
              FileType::DIRECTORY,
              0b111),
          TreeEntry(
              Hash("4444444444444444444444444444444444444444"),
              "exec",
              FileType::REGULAR_FILE,
              0b111),
          TreeEntry(
              Hash("5555555555555555555555555555555555555555"),
              "link",
              FileType::SYMLINK,
              0b111),
      };

      Tree expected_tree(std::move(expected_entries), treehash);
      EXPECT_TRUE(expected_tree == *tree);
      server->stop();
    });
  });

  t.join();
}
