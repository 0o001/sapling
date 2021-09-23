/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include <folly/executors/QueuedImmediateExecutor.h>
#include <folly/portability/GTest.h>
#include <folly/test/TestUtils.h>

#include "eden/fs/store/MemoryLocalStore.h"
#include "eden/fs/store/ObjectFetchContext.h"
#include "eden/fs/store/ObjectStore.h"
#include "eden/fs/telemetry/NullStructuredLogger.h"
#include "eden/fs/testharness/FakeBackingStore.h"
#include "eden/fs/testharness/LoggingFetchContext.h"
#include "eden/fs/testharness/StoredObject.h"

using namespace facebook::eden;
using namespace folly::string_piece_literals;
using namespace std::chrono_literals;

namespace {

constexpr size_t kTreeCacheMaximumSize = 1000; // bytes
constexpr size_t kTreeCacheMinimumEntries = 0;

struct ObjectStoreTest : ::testing::Test {
  void SetUp() override {
    std::shared_ptr<EdenConfig> rawEdenConfig{
        EdenConfig::createTestEdenConfig()};
    rawEdenConfig->inMemoryTreeCacheSize.setValue(
        kTreeCacheMaximumSize, ConfigSource::Default, true);
    rawEdenConfig->inMemoryTreeCacheMinElements.setValue(
        kTreeCacheMinimumEntries, ConfigSource::Default, true);
    auto edenConfig = std::make_shared<ReloadableConfig>(
        rawEdenConfig, ConfigReloadBehavior::NoReload);
    treeCache = TreeCache::create(edenConfig);
    localStore = std::make_shared<MemoryLocalStore>();
    backingStore = std::make_shared<FakeBackingStore>();
    stats = std::make_shared<EdenStats>();
    executor = &folly::QueuedImmediateExecutor::instance();
    objectStore = ObjectStore::create(
        localStore,
        backingStore,
        treeCache,
        stats,
        executor,
        std::make_shared<ProcessNameCache>(),
        std::make_shared<NullStructuredLogger>(),
        EdenConfig::createTestEdenConfig());

    readyBlobId = putReadyBlob("readyblob");
    readyTreeId = putReadyTree();
  }

  Hash putReadyBlob(folly::StringPiece data) {
    StoredBlob* storedBlob = backingStore->putBlob(data);
    storedBlob->setReady();
    return storedBlob->get().getHash();
  }

  Hash putReadyTree() {
    StoredTree* storedTree = backingStore->putTree({});
    storedTree->setReady();
    return storedTree->get().getHash();
  }

  LoggingFetchContext context;
  std::shared_ptr<LocalStore> localStore;
  std::shared_ptr<FakeBackingStore> backingStore;
  std::shared_ptr<TreeCache> treeCache;
  std::shared_ptr<EdenStats> stats;
  std::shared_ptr<ObjectStore> objectStore;
  folly::QueuedImmediateExecutor* executor;

  Hash readyBlobId;
  Hash readyTreeId;
};

} // namespace

TEST_F(ObjectStoreTest, getBlob_tracks_backing_store_read) {
  objectStore->getBlob(readyBlobId, context).get(0ms);
  ASSERT_EQ(1, context.requests.size());
  auto& request = context.requests[0];
  EXPECT_EQ(ObjectFetchContext::Blob, request.type);
  EXPECT_EQ(readyBlobId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromNetworkFetch, request.origin);
}

TEST_F(ObjectStoreTest, getBlob_tracks_second_read_from_cache) {
  objectStore->getBlob(readyBlobId, context).get(0ms);
  objectStore->getBlob(readyBlobId, context).get(0ms);
  ASSERT_EQ(2, context.requests.size());
  auto& request = context.requests[1];
  EXPECT_EQ(ObjectFetchContext::Blob, request.type);
  EXPECT_EQ(readyBlobId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromDiskCache, request.origin);
}

TEST_F(ObjectStoreTest, getTree_tracks_backing_store_read) {
  objectStore->getTree(readyTreeId, context).get(0ms);
  ASSERT_EQ(1, context.requests.size());
  auto& request = context.requests[0];
  EXPECT_EQ(ObjectFetchContext::Tree, request.type);
  EXPECT_EQ(readyTreeId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromNetworkFetch, request.origin);
}

TEST_F(ObjectStoreTest, getTree_tracks_second_read_from_cache) {
  objectStore->getTree(readyTreeId, context).get(0ms);
  objectStore->getTree(readyTreeId, context).get(0ms);
  ASSERT_EQ(2, context.requests.size());
  auto& request = context.requests[1];
  EXPECT_EQ(ObjectFetchContext::Tree, request.type);
  EXPECT_EQ(readyTreeId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromMemoryCache, request.origin);
}

TEST_F(ObjectStoreTest, getTree_tracks_second_read_from_local_store) {
  objectStore->getTree(readyTreeId, context).get(0ms);

  // clear the in memory cache so the tree can not be found here
  treeCache->clear();

  objectStore->getTree(readyTreeId, context).get(0ms);
  ASSERT_EQ(2, context.requests.size());
  auto& request = context.requests[1];
  EXPECT_EQ(ObjectFetchContext::Tree, request.type);
  EXPECT_EQ(readyTreeId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromDiskCache, request.origin);
}

TEST_F(ObjectStoreTest, getBlobSize_tracks_backing_store_read) {
  objectStore->getBlobSize(readyBlobId, context).get(0ms);
  ASSERT_EQ(1, context.requests.size());
  auto& request = context.requests[0];
  EXPECT_EQ(ObjectFetchContext::BlobMetadata, request.type);
  EXPECT_EQ(readyBlobId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromNetworkFetch, request.origin);
}

TEST_F(ObjectStoreTest, getBlobSize_tracks_second_read_from_cache) {
  objectStore->getBlobSize(readyBlobId, context).get(0ms);
  objectStore->getBlobSize(readyBlobId, context).get(0ms);
  ASSERT_EQ(2, context.requests.size());
  auto& request = context.requests[1];
  EXPECT_EQ(ObjectFetchContext::BlobMetadata, request.type);
  EXPECT_EQ(readyBlobId, request.hash);
  EXPECT_EQ(ObjectFetchContext::FromMemoryCache, request.origin);
}

TEST_F(ObjectStoreTest, getBlobSizeFromLocalStore) {
  auto data = "A"_sp;
  Hash id = putReadyBlob(data);

  // Get blob size from backing store, caches in local store
  objectStore->getBlobSize(id, context);
  // Clear backing store
  objectStore = ObjectStore::create(
      localStore,
      nullptr,
      treeCache,
      stats,
      executor,
      std::make_shared<ProcessNameCache>(),
      std::make_shared<NullStructuredLogger>(),
      EdenConfig::createTestEdenConfig());

  size_t expectedSize = data.size();
  size_t size = objectStore->getBlobSize(id, context).get();
  EXPECT_EQ(expectedSize, size);
}

TEST_F(ObjectStoreTest, getBlobSizeFromBackingStore) {
  auto data = "A"_sp;
  Hash id = putReadyBlob(data);

  size_t expectedSize = data.size();
  size_t size = objectStore->getBlobSize(id, context).get();
  EXPECT_EQ(expectedSize, size);
}

TEST_F(ObjectStoreTest, getBlobSizeNotFound) {
  Hash id;

  EXPECT_THROW_RE(
      objectStore->getBlobSize(id, context).get(),
      std::domain_error,
      "blob .* not found");
}

TEST_F(ObjectStoreTest, getBlobSha1) {
  auto data = "A"_sp;
  Hash id = putReadyBlob(data);

  Hash expectedSha1 = Hash::sha1(data);
  Hash sha1 = objectStore->getBlobSha1(id, context).get();
  EXPECT_EQ(expectedSha1.toString(), sha1.toString());
}

TEST_F(ObjectStoreTest, getBlobSha1NotFound) {
  Hash id;

  EXPECT_THROW_RE(
      objectStore->getBlobSha1(id, context).get(),
      std::domain_error,
      "blob .* not found");
}

TEST_F(ObjectStoreTest, get_size_and_sha1_only_imports_blob_once) {
  objectStore->getBlobSize(readyBlobId, context).get(0ms);
  objectStore->getBlobSha1(readyBlobId, context).get(0ms);

  EXPECT_EQ(1, backingStore->getAccessCount(readyBlobId));
}

class PidFetchContext : public ObjectFetchContext {
 public:
  PidFetchContext(pid_t pid) : ObjectFetchContext{}, pid_{pid} {}

  std::optional<pid_t> getClientPid() const override {
    return pid_;
  }

 private:
  pid_t pid_;
};

TEST_F(ObjectStoreTest, test_process_access_counts) {
  pid_t pid0{10000};
  PidFetchContext pidContext0{pid0};
  pid_t pid1{10001};
  PidFetchContext pidContext1{pid1};

  // first fetch increments fetch count for pid0
  objectStore->getBlob(readyBlobId, pidContext0).get(0ms);
  EXPECT_EQ(1, objectStore->getPidFetches().rlock()->at(pid0));

  // local fetch also increments fetch count for pid0
  objectStore->getBlob(readyBlobId, pidContext0).get(0ms);
  EXPECT_EQ(2, objectStore->getPidFetches().rlock()->at(pid0));

  // increments fetch count for pid1
  objectStore->getBlob(readyBlobId, pidContext1).get(0ms);
  EXPECT_EQ(2, objectStore->getPidFetches().rlock()->at(pid0));
  EXPECT_EQ(1, objectStore->getPidFetches().rlock()->at(pid1));
}
