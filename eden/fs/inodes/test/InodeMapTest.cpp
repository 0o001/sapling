/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/inodes/InodeMap.h"

#include <folly/Format.h>
#include <folly/String.h>
#include <folly/test/TestUtils.h>
#include <gtest/gtest.h>

#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/Overlay.h"
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/testharness/FakeTreeBuilder.h"
#include "eden/fs/testharness/TestMount.h"
#include "eden/fs/testharness/TestUtil.h"
#include "eden/fs/utils/Bug.h"

using namespace facebook::eden;
using folly::StringPiece;

TEST(InodeMap, invalidInodeNumber) {
  FakeTreeBuilder builder;
  builder.setFile("Makefile", "all:\necho success\n");
  builder.setFile("src/noop.c", "int main() { return 0; }\n");
  TestMount testMount{builder};

  EdenBugDisabler noCrash;
  auto* inodeMap = testMount.getEdenMount()->getInodeMap();
  auto future = inodeMap->lookupFileInode(0x12345678_ino);
  EXPECT_THROW_RE(future.get(), std::runtime_error, "unknown inode number");
}

TEST(InodeMap, simpleLookups) {
  // Test simple lookups that succeed immediately from the LocalStore
  FakeTreeBuilder builder;
  builder.setFile("Makefile", "all:\necho success\n");
  builder.setFile("src/noop.c", "int main() { return 0; }\n");
  TestMount testMount{builder};
  auto* inodeMap = testMount.getEdenMount()->getInodeMap();

  // Look up the tree inode by name first
  auto root = testMount.getEdenMount()->getRootInode();
  auto srcTree = root->getOrLoadChild(PathComponentPiece{"src"}).get();

  // Next look up the tree by inode number
  auto tree2 = inodeMap->lookupTreeInode(srcTree->getNodeId()).get();
  EXPECT_EQ(srcTree, tree2);
  EXPECT_EQ(RelativePath{"src"}, tree2->getPath());

  // Next look up src/noop.c by name
  auto noop = tree2->getOrLoadChild(PathComponentPiece{"noop.c"}).get();
  EXPECT_NE(srcTree->getNodeId(), noop->getNodeId());

  // And look up src/noop.c by inode ID
  auto noop2 = inodeMap->lookupFileInode(noop->getNodeId()).get();
  EXPECT_EQ(noop, noop2);
  EXPECT_EQ(RelativePath{"src/noop.c"}, noop2->getPath());

  // lookupTreeInode() and lookupFileInode() should fail
  // when called on the wrong file type.
  EXPECT_THROW_ERRNO(
      inodeMap->lookupFileInode(srcTree->getNodeId()).get(), EISDIR);
  EXPECT_THROW_ERRNO(
      inodeMap->lookupTreeInode(noop->getNodeId()).get(), ENOTDIR);
}

TEST(InodeMap, asyncLookup) {
  auto builder = FakeTreeBuilder();
  builder.setFile("README", "docs go here\n");
  builder.setFile("src/runme.sh", "#!/bin/sh\necho hello world\n", true);
  builder.setFile("src/test.txt", "this is a test file");
  TestMount testMount{builder, false};

  // Look up the "src" tree inode by name
  // The future should only be fulfilled when after we make the tree ready
  auto rootInode = testMount.getEdenMount()->getRootInode();
  auto srcFuture = rootInode->getOrLoadChild(PathComponentPiece{"src"});
  EXPECT_FALSE(srcFuture.isReady());

  // Start a second lookup before the first is ready
  auto srcFuture2 = rootInode->getOrLoadChild(PathComponentPiece{"src"});
  EXPECT_FALSE(srcFuture2.isReady());

  // Now make the tree ready
  builder.setReady("src");
  ASSERT_TRUE(srcFuture.isReady());
  ASSERT_TRUE(srcFuture2.isReady());
  auto srcTree = srcFuture.get(std::chrono::seconds(1));
  auto srcTree2 = srcFuture2.get(std::chrono::seconds(1));
  EXPECT_EQ(srcTree.get(), srcTree2.get());
}

TEST(InodeMap, asyncError) {
  auto builder = FakeTreeBuilder();
  builder.setFile("README", "docs go here\n");
  builder.setFile("src/runme.sh", "#!/bin/sh\necho hello world\n", true);
  builder.setFile("src/test.txt", "this is a test file");
  TestMount testMount{builder, false};

  // Look up the "src" tree inode by name
  // The future should only be fulfilled when after we make the tree ready
  auto rootInode = testMount.getEdenMount()->getRootInode();
  auto srcFuture = rootInode->getOrLoadChild(PathComponentPiece{"src"});
  EXPECT_FALSE(srcFuture.isReady());

  // Start a second lookup before the first is ready
  auto srcFuture2 = rootInode->getOrLoadChild(PathComponentPiece{"src"});
  EXPECT_FALSE(srcFuture2.isReady());

  // Now fail the tree lookup
  builder.triggerError(
      "src", std::domain_error("rejecting lookup for src tree"));
  ASSERT_TRUE(srcFuture.isReady());
  ASSERT_TRUE(srcFuture2.isReady());
  EXPECT_THROW(srcFuture.get(), std::domain_error);
  EXPECT_THROW(srcFuture2.get(), std::domain_error);
}

TEST(InodeMap, recursiveLookup) {
  auto builder = FakeTreeBuilder();
  builder.setFile("a/b/c/d/file.txt", "this is a test file");
  TestMount testMount{builder, false};
  const auto& edenMount = testMount.getEdenMount();

  // Call EdenMount::getInode() on the root
  auto rootFuture = edenMount->getInode(RelativePathPiece{""});
  ASSERT_TRUE(rootFuture.isReady());
  auto rootResult = rootFuture.get();
  EXPECT_EQ(edenMount->getRootInode(), rootResult);

  // Call EdenMount::getInode() to do a recursive lookup
  auto fileFuture = edenMount->getInode(RelativePathPiece{"a/b/c/d/file.txt"});
  EXPECT_FALSE(fileFuture.isReady());

  builder.setReady("a/b/c");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b/c/d/file.txt");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b/c/d");
  ASSERT_TRUE(fileFuture.isReady());
  auto fileInode = fileFuture.get();
  EXPECT_EQ(
      RelativePathPiece{"a/b/c/d/file.txt"}, fileInode->getPath().value());
}

TEST(InodeMap, recursiveLookupError) {
  auto builder = FakeTreeBuilder();
  builder.setFile("a/b/c/d/file.txt", "this is a test file");
  TestMount testMount{builder, false};
  const auto& edenMount = testMount.getEdenMount();

  // Call EdenMount::getInode() on the root
  auto rootFuture = edenMount->getInode(RelativePathPiece{""});
  ASSERT_TRUE(rootFuture.isReady());
  auto rootResult = rootFuture.get();
  EXPECT_EQ(edenMount->getRootInode(), rootResult);

  // Call EdenMount::getInode() to do a recursive lookup
  auto fileFuture = edenMount->getInode(RelativePathPiece{"a/b/c/d/file.txt"});
  EXPECT_FALSE(fileFuture.isReady());

  builder.setReady("a");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b/c");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b/c/d/file.txt");
  EXPECT_FALSE(fileFuture.isReady());
  builder.triggerError(
      "a/b/c/d", std::domain_error("error for testing purposes"));
  ASSERT_TRUE(fileFuture.isReady());
  EXPECT_THROW_RE(
      fileFuture.get(), std::domain_error, "error for testing purposes");
}

TEST(InodeMap, renameDuringRecursiveLookup) {
  auto builder = FakeTreeBuilder();
  builder.setFile("a/b/c/d/file.txt", "this is a test file");
  TestMount testMount{builder, false};
  const auto& edenMount = testMount.getEdenMount();

  // Call EdenMount::getInode() on the root
  auto rootFuture = edenMount->getInode(RelativePathPiece{""});
  ASSERT_TRUE(rootFuture.isReady());
  auto rootResult = rootFuture.get();
  EXPECT_EQ(edenMount->getRootInode(), rootResult);

  // Call EdenMount::getInode() to do a recursive lookup
  auto fileFuture = edenMount->getInode(RelativePathPiece{"a/b/c/d/file.txt"});
  EXPECT_FALSE(fileFuture.isReady());

  builder.setReady("a/b/c");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b");
  EXPECT_FALSE(fileFuture.isReady());

  auto bFuture = edenMount->getInode(RelativePathPiece{"a/b"});
  ASSERT_TRUE(bFuture.isReady());
  auto bInode = bFuture.get().asTreePtr();

  // Rename c to x after the recursive resolution should have
  // already looked it up
  auto renameFuture =
      bInode->rename(PathComponentPiece{"c"}, bInode, PathComponentPiece{"x"});
  ASSERT_TRUE(renameFuture.isReady());
  EXPECT_FALSE(fileFuture.isReady());

  // Now mark the rest of the tree ready
  // Note that we don't actually have to mark the file itself ready.
  // The Inode lookup itself doesn't need the blob data yet.
  builder.setReady("a/b/c/d");
  ASSERT_TRUE(fileFuture.isReady());
  auto fileInode = fileFuture.get();
  // We should have successfully looked up the inode, but it will report it
  // self (correctly) at its new path now.
  EXPECT_EQ(
      RelativePathPiece{"a/b/x/d/file.txt"}, fileInode->getPath().value());
}

TEST(InodeMap, renameDuringRecursiveLookupAndLoad) {
  auto builder = FakeTreeBuilder();
  builder.setFile("a/b/c/d/file.txt", "this is a test file");
  TestMount testMount{builder, false};
  const auto& edenMount = testMount.getEdenMount();

  // Call EdenMount::getInode() on the root
  auto rootFuture = edenMount->getInode(RelativePathPiece{""});
  ASSERT_TRUE(rootFuture.isReady());
  auto rootResult = rootFuture.get();
  EXPECT_EQ(edenMount->getRootInode(), rootResult);

  // Call EdenMount::getInode() to do a recursive lookup
  auto fileFuture = edenMount->getInode(RelativePathPiece{"a/b/c/d/file.txt"});
  EXPECT_FALSE(fileFuture.isReady());

  builder.setReady("a");
  EXPECT_FALSE(fileFuture.isReady());
  builder.setReady("a/b");
  EXPECT_FALSE(fileFuture.isReady());

  auto bFuture = edenMount->getInode(RelativePathPiece{"a/b"});
  ASSERT_TRUE(bFuture.isReady());
  auto bInode = bFuture.get().asTreePtr();

  // Rename c to x while the recursive resolution is still trying
  // to look it up.
  auto renameFuture =
      bInode->rename(PathComponentPiece{"c"}, bInode, PathComponentPiece{"x"});
  // The rename will not complete until C becomes ready
  EXPECT_FALSE(renameFuture.isReady());
  EXPECT_FALSE(fileFuture.isReady());

  builder.setReady("a/b/c");
  ASSERT_TRUE(renameFuture.isReady());
  EXPECT_FALSE(fileFuture.isReady());

  // Now mark the rest of the tree ready
  // Note that we don't actually have to mark the file itself ready.
  // The Inode lookup itself doesn't need the blob data yet.
  builder.setReady("a/b/c/d");
  ASSERT_TRUE(fileFuture.isReady());
  auto fileInode = fileFuture.get();
  // We should have successfully looked up the inode, but it will report it
  // self (correctly) at its new path now.
  EXPECT_EQ(
      RelativePathPiece{"a/b/x/d/file.txt"}, fileInode->getPath().value());
}

TEST(InodeMap, unloadedUnlinkedTreesAreRemovedFromOverlay) {
  FakeTreeBuilder builder;
  builder.setFile("dir1/file.txt", "contents");
  builder.setFile("dir2/file.txt", "contents");
  TestMount mount{builder};
  auto edenMount = mount.getEdenMount();

  auto root = edenMount->getRootInode();
  auto dir1 = edenMount->getInode(RelativePathPiece{"dir1"}).get().asTreePtr();
  auto dir2 = edenMount->getInode(RelativePathPiece{"dir2"}).get().asTreePtr();

  auto dir1ino = dir1->getNodeId();
  auto dir2ino = dir2->getNodeId();

  dir1->unlink(PathComponentPiece{"file.txt"}).get();
  dir2->unlink(PathComponentPiece{"file.txt"}).get();

  // Test both having a positive and zero fuse reference counts.
  dir2->incFuseRefcount();

  root->rmdir(PathComponentPiece{"dir1"});
  root->rmdir(PathComponentPiece{"dir2"});

  dir1.reset();
  dir2.reset();

  edenMount->getInodeMap()->decFuseRefcount(dir2ino);
  EXPECT_FALSE(edenMount->getOverlay()->hasOverlayData(dir1ino));
  EXPECT_FALSE(edenMount->getOverlay()->hasOverlayData(dir2ino));
}

struct InodePersistenceTreeTest : ::testing::Test {
  InodePersistenceTreeTest() {
    builder.setFile("dir/file1.txt", "contents1");
    builder.setFile("dir/file2.txt", "contents2");
  }

  FakeTreeBuilder builder;
};

struct InodePersistenceTakeoverTest : InodePersistenceTreeTest {
  InodePersistenceTakeoverTest()
      : testMount{builder}, edenMount{testMount.getEdenMount()} {}

  void SetUp() override {
    InodePersistenceTreeTest::SetUp();

    auto tree = edenMount->getInode(RelativePathPiece{"dir"}).get();
    auto file1 = edenMount->getInode(RelativePathPiece{"dir/file1.txt"}).get();
    auto file2 = edenMount->getInode(RelativePathPiece{"dir/file2.txt"}).get();

    // Pretend FUSE is keeping references to these.
    tree->incFuseRefcount();
    file1->incFuseRefcount();
    file2->incFuseRefcount();

    oldTreeId = tree->getNodeId();
    oldFile1Id = file1->getNodeId();
    oldFile2Id = file2->getNodeId();

    edenMount.reset();
    tree.reset();
    file1.reset();
    file2.reset();
    testMount.remountGracefully();

    edenMount = testMount.getEdenMount();
  }

  TestMount testMount;
  std::shared_ptr<EdenMount> edenMount;

  InodeNumber oldTreeId;
  InodeNumber oldFile1Id;
  InodeNumber oldFile2Id;
};

TEST_F(
    InodePersistenceTakeoverTest,
    preservesInodeNumbersForLoadedInodesDuringTakeover_lookupFirstByName) {
  // Look up in a different order to avoid allocating the same numbers.
  auto tree = edenMount->getInode(RelativePathPiece{"dir"}).get();
  auto file2 = edenMount->getInode(RelativePathPiece{"dir/file2.txt"}).get();
  auto file1 = edenMount->getInode(RelativePathPiece{"dir/file1.txt"}).get();

  EXPECT_EQ(1, tree->debugGetFuseRefcount());
  EXPECT_EQ(1, file1->debugGetFuseRefcount());
  EXPECT_EQ(1, file2->debugGetFuseRefcount());

  EXPECT_EQ(oldTreeId, tree->getNodeId());
  EXPECT_EQ(oldFile1Id, file1->getNodeId());
  EXPECT_EQ(oldFile2Id, file2->getNodeId());

  // Now try looking up by inode number.
  EXPECT_EQ(
      "dir",
      edenMount->getInodeMap()->lookupInode(oldTreeId).get()->getLogPath());
  EXPECT_EQ(
      "dir/file1.txt",
      edenMount->getInodeMap()->lookupInode(oldFile1Id).get()->getLogPath());
  EXPECT_EQ(
      "dir/file2.txt",
      edenMount->getInodeMap()->lookupInode(oldFile2Id).get()->getLogPath());
}

TEST_F(
    InodePersistenceTakeoverTest,
    preservesInodeNumbersForLoadedInodesDuringTakeover_lookupFirstByNumber) {
  // Look up by number first.
  EXPECT_EQ(
      "dir",
      edenMount->getInodeMap()->lookupInode(oldTreeId).get()->getLogPath());
  EXPECT_EQ(
      "dir/file1.txt",
      edenMount->getInodeMap()->lookupInode(oldFile1Id).get()->getLogPath());
  EXPECT_EQ(
      "dir/file2.txt",
      edenMount->getInodeMap()->lookupInode(oldFile2Id).get()->getLogPath());

  // Verify the same inodes can be looked up by name too.
  auto tree = edenMount->getInode(RelativePathPiece{"dir"}).get();
  auto file2 = edenMount->getInode(RelativePathPiece{"dir/file2.txt"}).get();
  auto file1 = edenMount->getInode(RelativePathPiece{"dir/file1.txt"}).get();

  EXPECT_EQ(1, tree->debugGetFuseRefcount());
  EXPECT_EQ(1, file1->debugGetFuseRefcount());
  EXPECT_EQ(1, file2->debugGetFuseRefcount());

  EXPECT_EQ(oldTreeId, tree->getNodeId());
  EXPECT_EQ(oldFile1Id, file1->getNodeId());
  EXPECT_EQ(oldFile2Id, file2->getNodeId());
}

/**
 * clang and gcc use the inode number of a header to determine whether it's the
 * same file as one previously included and marked #pragma once.
 *
 * At least as long as the mount is up (including though graceful takeovers),
 * Eden must provide consistent inode numbers.
 */
TEST_F(
    InodePersistenceTreeTest,
    preservesInodeNumbersForUnloadedInodesDuringTakeover) {
  TestMount testMount{builder};
  auto edenMount = testMount.getEdenMount();

  auto tree = edenMount->getInode(RelativePathPiece{"dir"}).get();
  auto file1 = edenMount->getInode(RelativePathPiece{"dir/file1.txt"}).get();
  auto file2 = edenMount->getInode(RelativePathPiece{"dir/file2.txt"}).get();

  tree->incFuseRefcount();
  file1->incFuseRefcount();
  file2->incFuseRefcount();

  auto oldTreeId = tree->getNodeId();
  auto oldFile1Id = file1->getNodeId();
  auto oldFile2Id = file2->getNodeId();

  tree->decFuseRefcount();
  file1->decFuseRefcount();
  file2->decFuseRefcount();

  edenMount.reset();
  tree.reset();
  file1.reset();
  file2.reset();
  testMount.remountGracefully();

  edenMount = testMount.getEdenMount();

  // Look up in a different order.
  tree = edenMount->getInode(RelativePathPiece{"dir"}).get();
  file2 = edenMount->getInode(RelativePathPiece{"dir/file2.txt"}).get();
  file1 = edenMount->getInode(RelativePathPiece{"dir/file1.txt"}).get();

  EXPECT_EQ(oldTreeId, tree->getNodeId());
  EXPECT_EQ(oldFile1Id, file1->getNodeId());
  EXPECT_EQ(oldFile2Id, file2->getNodeId());
}
