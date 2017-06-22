/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/futures/Future.h>
#include <folly/io/IOBuf.h>
#include <gtest/gtest.h>
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/testharness/FakeObjectStore.h"

using namespace facebook::eden;
using folly::IOBuf;
using std::unique_ptr;
using std::unordered_map;
using std::vector;

namespace {
Hash fileHash("0000000000000000000000000000000000000000");
Hash tree1Hash("1111111111111111111111111111111111111111");
Hash tree2Hash("2222222222222222222222222222222222222222");
Hash commHash("4444444444444444444444444444444444444444");
Hash blobHash("5555555555555555555555555555555555555555");
}

TEST(FakeObjectStore, getObjectsOfAllTypesFromStore) {
  FakeObjectStore store;

  // Test getTreeFuture().
  vector<TreeEntry> entries1;
  uint8_t rw_ = 0b110;
  entries1.emplace_back(fileHash, "a_file", FileType::REGULAR_FILE, rw_);
  Tree tree1(std::move(entries1), tree1Hash);
  store.addTree(std::move(tree1));
  auto foundTree = store.getTreeFuture(tree1Hash).get();
  EXPECT_TRUE(foundTree);
  EXPECT_EQ(tree1Hash, foundTree->getHash());

  // Test getBlobFuture().
  auto buf1 = IOBuf();
  Blob blob1(blobHash, buf1);
  store.addBlob(std::move(blob1));
  auto foundBlob = store.getBlobFuture(blobHash).get();
  EXPECT_TRUE(foundBlob);
  EXPECT_EQ(blobHash, foundBlob->getHash());

  // Test getTreeForCommit().
  vector<TreeEntry> entries2;
  entries2.emplace_back(fileHash, "a_file", FileType::REGULAR_FILE, rw_);
  Tree tree2(std::move(entries2), tree2Hash);
  store.setTreeForCommit(commHash, std::move(tree2));
  auto foundTreeForCommit = store.getTreeForCommit(commHash).get();
  ASSERT_NE(nullptr, foundTreeForCommit.get());
  EXPECT_EQ(tree2Hash, foundTreeForCommit->getHash());

  // Test getBlobMetadata() and getSha1ForBlob().
  auto buf2 = IOBuf();
  Blob blob2(blobHash, buf2);
  auto expectedSha1 = Hash::sha1(&buf1);
  auto foundSha1 = store.getSha1ForBlob(blob2.getHash());
  EXPECT_EQ(expectedSha1, foundSha1);
  auto metadataFuture = store.getBlobMetadata(blob2.getHash());
  ASSERT_TRUE(metadataFuture.isReady());
  auto metadata = metadataFuture.get();
  EXPECT_EQ(expectedSha1, metadata.sha1);
  EXPECT_EQ(0, metadata.size);
}

TEST(FakeObjectStore, getMissingObjectThrows) {
  FakeObjectStore store;
  Hash hash("4242424242424242424242424242424242424242");
  EXPECT_THROW(store.getTreeFuture(hash).get(), std::domain_error);
  EXPECT_THROW(store.getBlobFuture(hash).get(), std::domain_error);
  EXPECT_THROW(store.getTreeForCommit(hash).get(), std::domain_error);
  EXPECT_THROW(store.getSha1ForBlob(hash), std::domain_error);
}
