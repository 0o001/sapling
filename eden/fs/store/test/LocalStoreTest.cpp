/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/store/LocalStore.h"
#include <folly/Optional.h>
#include <folly/String.h>
#include <folly/experimental/TestUtil.h>
#include <folly/io/IOBuf.h>
#include <gtest/gtest.h>
#include <stdexcept>
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/store/MemoryLocalStore.h"
#include "eden/fs/store/RocksDbLocalStore.h"
#include "eden/fs/store/SqliteLocalStore.h"
#include "eden/fs/store/StoreResult.h"

using namespace facebook::eden;

using folly::IOBuf;
using folly::StringPiece;
using folly::unhexlify;
using folly::test::TemporaryDirectory;
using std::string;
using KeySpace = facebook::eden::LocalStore::KeySpace;

enum class StoreImpl {
  Memory,
  RocksDB,
  Sqlite,
};

class LocalStoreTest : public ::testing::TestWithParam<StoreImpl> {
 protected:
  void SetUp() override {
    switch (GetParam()) {
      case StoreImpl::Memory:
        store_ = std::make_unique<MemoryLocalStore>();
        break;
      case StoreImpl::RocksDB:
        testDir_ = std::make_unique<TemporaryDirectory>("eden_test");
        store_ = std::make_unique<RocksDbLocalStore>(
            AbsolutePathPiece{testDir_->path().string()});
        break;
      case StoreImpl::Sqlite:
        testDir_ = std::make_unique<TemporaryDirectory>("eden_test");
        store_ = std::make_unique<SqliteLocalStore>(
            AbsolutePathPiece{testDir_->path().string()} +
            PathComponentPiece("sqlite"));
    }
  }

  void TearDown() override {
    store_.reset();
    testDir_.reset();
  }

  std::unique_ptr<TemporaryDirectory> testDir_;
  std::unique_ptr<LocalStore> store_;
};

TEST_P(LocalStoreTest, testReadAndWriteBlob) {
  Hash hash("3a8f8eb91101860fd8484154885838bf322964d0");

  StringPiece contents("{\n  \"breakConfig\": true\n}\n");
  auto buf = IOBuf{IOBuf::WRAP_BUFFER, folly::ByteRange{contents}};
  auto sha1 = Hash::sha1(&buf);

  auto inBlob = Blob{hash, std::move(buf)};
  store_->putBlob(hash, &inBlob);

  auto outBlob = store_->getBlob(hash);
  EXPECT_EQ(hash, outBlob->getHash());
  EXPECT_EQ(
      contents, outBlob->getContents().clone()->moveToFbString().toStdString());

  auto retreivedMetadata = store_->getBlobMetadata(hash);
  ASSERT_TRUE(retreivedMetadata.hasValue());
  EXPECT_EQ(sha1, retreivedMetadata.value().sha1);
  EXPECT_EQ(contents.size(), retreivedMetadata.value().size);
}

TEST_P(LocalStoreTest, testReadNonexistent) {
  Hash hash("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
  EXPECT_TRUE(nullptr == store_->getBlob(hash));
  auto retreivedMetadata = store_->getBlobMetadata(hash);
  EXPECT_FALSE(retreivedMetadata.hasValue());
}

TEST_P(LocalStoreTest, testReadsAndWriteTree) {
  Hash hash(folly::StringPiece{"8e073e366ed82de6465d1209d3f07da7eebabb93"});

  auto gitTreeObject = folly::to<string>(
      string("tree 424\x00", 9),

      string("100644 .babelrc\x00", 16),
      unhexlify("3a8f8eb91101860fd8484154885838bf322964d0"),

      string("100644 .flowconfig\x00", 19),
      unhexlify("3610882f48696cc7ca0835929511c9db70acbec6"),

      string("100644 README.md\x00", 17),
      unhexlify("c5f15617ed29cd35964dc197a7960aeaedf2c2d5"),

      string("40000 lib\x00", 10),
      unhexlify("e95798e17f694c227b7a8441cc5c7dae50a187d0"),

      string("100755 nuclide-start-server\x00", 28),
      unhexlify("006babcf5734d028098961c6f4b6b6719656924b"),

      string("100644 package.json\x00", 20),
      unhexlify("582591e0f0d92cb63a85156e39abd43ebf103edc"),

      string("40000 scripts\x00", 14),
      unhexlify("e664fd28e60a0da25739fdf732f412ab3e91d1e1"),

      string("100644 services-3.json\x00", 23),
      unhexlify("3ead3c6cd723f4867bef4444ba18e6ffbf0f711a"),

      string("100644 services-config.json\x00", 28),
      unhexlify("bbc8e67499b7f3e1ea850eeda1253be7da5c9199"),

      string("40000 spec\x00", 11),
      unhexlify("3bae53a99d080dd851f78e36eb343320091a3d57"),

      string("100644 xdebug.ini\x00", 18),
      unhexlify("9ed5bbccd1b9b0077561d14c0130dc086ab27e04"));

  store_->put(
      KeySpace::TreeFamily, hash.getBytes(), folly::StringPiece{gitTreeObject});
  auto tree = store_->getTree(hash);
  EXPECT_EQ(Hash("8e073e366ed82de6465d1209d3f07da7eebabb93"), tree->getHash());
  EXPECT_EQ(11, tree->getTreeEntries().size());

  auto readmeEntry = tree->getEntryAt(2);
  EXPECT_EQ(
      Hash("c5f15617ed29cd35964dc197a7960aeaedf2c2d5"), readmeEntry.getHash());
  EXPECT_EQ("README.md", readmeEntry.getName());
  EXPECT_EQ(TreeEntryType::BLOB, readmeEntry.getType());
  EXPECT_EQ(FileType::REGULAR_FILE, readmeEntry.getFileType());
  EXPECT_EQ(0b0110, readmeEntry.getOwnerPermissions());
}

TEST_P(LocalStoreTest, testGetResult) {
  StringPiece key1 = "foo";
  StringPiece key2 = "bar";

  EXPECT_FALSE(store_->get(KeySpace::BlobFamily, key1).isValid());
  EXPECT_FALSE(store_->get(KeySpace::BlobFamily, key2).isValid());

  store_->put(KeySpace::BlobFamily, key1, StringPiece{"hello world"});
  auto result1 = store_->get(KeySpace::BlobFamily, key1);
  ASSERT_TRUE(result1.isValid());
  EXPECT_EQ("hello world", result1.piece());

  auto result2 = store_->get(KeySpace::BlobFamily, key2);
  EXPECT_FALSE(result2.isValid());
  EXPECT_THROW(result2.piece(), std::domain_error);
}

TEST_P(LocalStoreTest, testMultipleBlobWriters) {
  StringPiece key1_1 = "foo";
  StringPiece key1_2 = "bar";

  StringPiece key1_3 = "john";
  StringPiece key1_4 = "doe";

  StringPiece key2_1 = "bender";
  StringPiece key2_2 = "bending";

  StringPiece key3_1 = "max";
  StringPiece key3_2 = "damage";

  auto batch1 = store_->beginWrite(8192);
  batch1->put(KeySpace::BlobFamily, key1_1, StringPiece{"hello world1_1"});
  batch1->put(KeySpace::BlobFamily, key1_2, StringPiece{"hello world1_2"});

  auto batch2 = store_->beginWrite(1024);
  batch2->put(KeySpace::BlobFamily, key2_1, StringPiece{"hello world2_1"});
  batch2->put(KeySpace::BlobFamily, key2_2, StringPiece{"hello world2_2"});

  auto batch3 = store_->beginWrite();
  batch3->put(KeySpace::BlobFamily, key3_1, StringPiece{"hello world3_1"});
  batch3->put(KeySpace::BlobFamily, key3_2, StringPiece{"hello world3_2"});

  batch1->put(KeySpace::BlobFamily, key1_3, StringPiece{"hello world1_3"});
  batch1->put(KeySpace::BlobFamily, key1_4, StringPiece{"hello world1_4"});

  batch1->flush();
  batch2->flush();

  auto result1_1 = store_->get(KeySpace::BlobFamily, key1_1);
  auto result2_1 = store_->get(KeySpace::BlobFamily, key2_1);
  auto result1_3 = store_->get(KeySpace::BlobFamily, key1_3);
  auto result1_4 = store_->get(KeySpace::BlobFamily, key1_4);

  EXPECT_FALSE(store_->get(KeySpace::BlobFamily, key3_1).isValid())
      << "key3_1 is not visible until flush";
  batch3->flush();
  auto result3_1 = store_->get(KeySpace::BlobFamily, key3_1);
  EXPECT_EQ("hello world3_1", result3_1.piece())
      << "key3_1 visible after flush";

  EXPECT_EQ("hello world1_1", result1_1.piece());
  EXPECT_EQ("hello world2_1", result2_1.piece());
  EXPECT_EQ("hello world1_4", result1_4.piece());
}

INSTANTIATE_TEST_CASE_P(
    Memory,
    LocalStoreTest,
    ::testing::Values(StoreImpl::Memory));

INSTANTIATE_TEST_CASE_P(
    RocksDB,
    LocalStoreTest,
    ::testing::Values(StoreImpl::RocksDB));

INSTANTIATE_TEST_CASE_P(
    Sqlite,
    LocalStoreTest,
    ::testing::Values(StoreImpl::Sqlite));
