/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/Conv.h>
#include <folly/Optional.h>
#include <folly/chrono/Conv.h>
#include <folly/container/Array.h>
#include <folly/test/TestUtils.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/InodeMap.h"
#include "eden/fs/inodes/Overlay.h"
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/service/PrettyPrinters.h"
#include "eden/fs/testharness/FakeBackingStore.h"
#include "eden/fs/testharness/FakeTreeBuilder.h"
#include "eden/fs/testharness/TestChecks.h"
#include "eden/fs/testharness/TestMount.h"
#include "eden/fs/testharness/TestUtil.h"
#include "eden/fs/utils/TimeUtil.h"

using namespace facebook::eden;
using namespace std::chrono_literals;
using folly::Future;
using folly::makeFuture;
using folly::Optional;
using folly::StringPiece;
using folly::Unit;
using std::string;
using std::chrono::system_clock;
using testing::UnorderedElementsAre;

namespace std {
template <typename Rep, typename Period>
inline void PrintTo(
    std::chrono::duration<Rep, Period> duration,
    ::std::ostream* os) {
  *os << facebook::eden::durationStr(duration);
}

inline void PrintTo(
    const std::chrono::system_clock::time_point& t,
    ::std::ostream* os) {
  auto ts = folly::to<struct timespec>(t);

  struct tm localTime;
  if (!localtime_r(&ts.tv_sec, &localTime)) {
    folly::throwSystemError("localtime_r failed");
  }

  std::array<char, 64> buf;
  if (strftime(buf.data(), buf.size(), "%FT%T", &localTime) == 0) {
    // errno is not necessarily set
    throw std::runtime_error("strftime failed");
  }

  *os << buf.data() << folly::sformat(".{:09d}", ts.tv_nsec);
}
} // namespace std

namespace {

bool isExecutable(int perms) {
  return perms & S_IXUSR;
}

/**
 * An enum to control behavior for many of the checkout tests.
 *
 * Whether or not inodes are loaded when checkout runs affects which code
 * paths we hit, but it should not affect the user-visible behavior.
 */
enum class LoadBehavior {
  // None of the inodes in question are explicitly loaded
  // before the checkout operation.
  NONE,
  // Assign an inode number for the parent directory, but do not load it yet.
  ASSIGN_PARENT_INODE,
  // Load the parent TreeInode object before starting the checkout.
  PARENT,
  // Load the parent TreeInode object, and assign an inode number to the
  // child in question, but do not load the child InodeBase.
  ASSIGN_INODE,
  // Load the InodeBase affected by the test before starting the checkout.
  INODE,
  // Walk the tree and load every inode
  ALL,
};

static constexpr auto kAllLoadTypes = folly::make_array(
    LoadBehavior::NONE,
    LoadBehavior::ASSIGN_PARENT_INODE,
    LoadBehavior::PARENT,
    LoadBehavior::ASSIGN_INODE,
    LoadBehavior::INODE,
    LoadBehavior::ALL);

// LoadTypes that can be used with tests that add a new file
static constexpr auto kAddLoadTypes = folly::make_array(
    LoadBehavior::NONE,
    LoadBehavior::ASSIGN_PARENT_INODE,
    LoadBehavior::PARENT,
    LoadBehavior::ALL);

std::string loadBehaviorToString(LoadBehavior loadType) {
  switch (loadType) {
    case LoadBehavior::NONE:
      return "NONE";
    case LoadBehavior::ASSIGN_PARENT_INODE:
      return "ASSIGN_PARENT_INODE";
    case LoadBehavior::PARENT:
      return "PARENT";
    case LoadBehavior::ASSIGN_INODE:
      return "ASSIGN_INODE";
    case LoadBehavior::INODE:
      return "INODE";
    case LoadBehavior::ALL:
      return "ALL";
  }
  return folly::to<std::string>("<unknown LoadBehavior ", int(loadType), ">");
}

template <typename TargetType>
typename std::enable_if<folly::IsSomeString<TargetType>::value>::type toAppend(
    LoadBehavior loadType,
    TargetType* result) {
  result->append(loadBehaviorToString(loadType));
}

std::ostream& operator<<(std::ostream& os, LoadBehavior loadType) {
  os << loadBehaviorToString(loadType);
  return os;
}

void loadInodes(
    TestMount& testMount,
    RelativePathPiece path,
    LoadBehavior loadType,
    folly::Optional<folly::StringPiece> expectedContents,
    mode_t expectedPerms) {
  switch (loadType) {
    case LoadBehavior::NONE:
      return;
    case LoadBehavior::ASSIGN_PARENT_INODE: {
      // Load the parent TreeInode but not the affected file
      testMount.getTreeInode(path.dirname());
      auto parentPath = path.dirname();
      auto grandparentInode = testMount.getTreeInode(parentPath.dirname());
      grandparentInode->getChildInodeNumber(parentPath.basename());
      return;
    }
    case LoadBehavior::PARENT:
      // Load the parent TreeInode but not the affected file
      testMount.getTreeInode(path.dirname());
      return;
    case LoadBehavior::ASSIGN_INODE: {
      auto parent = testMount.getTreeInode(path.dirname());
      parent->getChildInodeNumber(path.basename());
      return;
    }
    case LoadBehavior::INODE: {
      if (expectedContents.hasValue()) {
        // The inode in question must be a file.  Load it and verify the
        // contents are what we expect.
        auto fileInode = testMount.getFileInode(path);
        EXPECT_FILE_INODE(fileInode, expectedContents.value(), expectedPerms);
      } else {
        // The inode might be a tree or a file.
        testMount.getInode(path);
      }
      return;
    }
    case LoadBehavior::ALL:
      testMount.loadAllInodes();
      return;
  }

  FAIL() << "unknown load behavior: " << loadType;
}

void loadInodes(
    TestMount& testMount,
    folly::StringPiece path,
    LoadBehavior loadType,
    folly::StringPiece expectedContents,
    mode_t expectedPerms = 0644) {
  loadInodes(
      testMount,
      RelativePathPiece{path},
      loadType,
      expectedContents,
      expectedPerms);
}

void loadInodes(
    TestMount& testMount,
    RelativePathPiece path,
    LoadBehavior loadType) {
  loadInodes(testMount, path, loadType, folly::none, 0644);
}

void loadInodes(
    TestMount& testMount,
    folly::StringPiece path,
    LoadBehavior loadType) {
  loadInodes(testMount, RelativePathPiece{path}, loadType, folly::none, 0644);
}

CheckoutConflict
makeConflict(ConflictType type, StringPiece path, StringPiece message = "") {
  CheckoutConflict conflict;
  conflict.type = type;
  conflict.path = path.str();
  conflict.message = message.str();
  return conflict;
}
} // unnamed namespace

void testAddFile(
    folly::StringPiece newFilePath,
    LoadBehavior loadType,
    int perms = 0644) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "int main() { return 0; }\n");
  builder1.setFile("src/test/test.c", "testy tests");
  TestMount testMount{builder1};

  // Prepare a second tree, by starting with builder1 then adding the new file
  auto builder2 = builder1.clone();
  builder2.setFile(
      newFilePath, "this is the new file contents\n", isExecutable(perms));
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  loadInodes(testMount, newFilePath, loadType);

  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  // Confirm that the tree has been updated correctly.
  auto newInode = testMount.getFileInode(newFilePath);
  EXPECT_FILE_INODE(newInode, "this is the new file contents\n", perms);

  // Unmount and remount the mount point, and verify that the new file
  // still exists as expected.
  newInode.reset();
  testMount.remount();
  newInode = testMount.getFileInode(newFilePath);
  EXPECT_FILE_INODE(newInode, "this is the new file contents\n", perms);
}

void runAddFileTests(folly::StringPiece path) {
  for (auto loadType : kAddLoadTypes) {
    SCOPED_TRACE(folly::to<string>("add ", path, " load type ", loadType));
    testAddFile(path, loadType);
    testAddFile(path, loadType, 0755);
  }
}

TEST(Checkout, addFile) {
  // Test with file names that will be at the beginning of the directory,
  // in the middle of the directory, and at the end of the directory.
  // (The directory entries are processed in sorted order.)
  runAddFileTests("src/aaa.c");
  runAddFileTests("src/ppp.c");
  runAddFileTests("src/zzz.c");
}

void testRemoveFile(folly::StringPiece filePath, LoadBehavior loadType) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "int main() { return 0; }\n");
  builder1.setFile("src/test/test.c", "testy tests");
  builder1.setFile(filePath, "this file will be removed\n");
  TestMount testMount{builder1};

  // Prepare a second tree, by starting with builder1 then removing the desired
  // file
  auto builder2 = builder1.clone();
  builder2.removeFile(filePath);
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  loadInodes(testMount, filePath, loadType, "this file will be removed\n");

  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  // Make sure the path doesn't exist any more.
  EXPECT_THROW_ERRNO(testMount.getInode(filePath), ENOENT);

  // Unmount and remount the mount point, and verify that the file removal
  // persisted across remount correctly.
  testMount.remount();
  EXPECT_THROW_ERRNO(testMount.getInode(filePath), ENOENT);
}

void runRemoveFileTests(folly::StringPiece path) {
  // Modify just the file contents, but not the permissions
  for (auto loadType : kAllLoadTypes) {
    SCOPED_TRACE(folly::to<string>("remove ", path, " load type ", loadType));
    testRemoveFile(path, loadType);
  }
}

TEST(Checkout, removeFile) {
  // Test with file names that will be at the beginning of the directory,
  // in the middle of the directory, and at the end of the directory.
  // (The directory entries are processed in sorted order.)
  runRemoveFileTests("src/aaa.c");
  runRemoveFileTests("src/ppp.c");
  runRemoveFileTests("src/zzz.c");
}

void testModifyFile(
    folly::StringPiece path,
    LoadBehavior loadType,
    folly::StringPiece contents1,
    int perms1,
    folly::StringPiece contents2,
    int perms2) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("readme.txt", "just filling out the tree\n");
  builder1.setFile("a/test.txt", "test contents\n");
  builder1.setFile("a/b/dddd.c", "this is dddd.c\n");
  builder1.setFile("a/b/tttt.c", "this is tttt.c\n");
  builder1.setFile(path, contents1, isExecutable(perms1));
  TestMount testMount{builder1};
  testMount.getClock().advance(9876min);

  // Prepare the second tree
  auto builder2 = builder1.clone();
  builder2.replaceFile(path, contents2, isExecutable(perms2));
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  loadInodes(testMount, path, loadType, contents1, perms1);

  Optional<struct stat> preStat;
  // If we were supposed to load this inode before the checkout,
  // also store its stat information so we can compare it after the checkout.
  if (loadType == LoadBehavior::INODE || loadType == LoadBehavior::ALL) {
    auto preInode = testMount.getFileInode(path);
    preStat = preInode->getattr().get(10ms).st;
  }

  testMount.getClock().advance(10min);
  auto checkoutStart = testMount.getClock().getTimePoint();
  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  // Make sure the path is updated as expected
  auto postInode = testMount.getFileInode(path);
  EXPECT_FILE_INODE(postInode, contents2, perms2);

  // Check the stat() information on the inode.
  // The timestamps should not be earlier than when the checkout started.
  auto postStat = postInode->getattr().get(10ms).st;
  EXPECT_GE(
      folly::to<system_clock::time_point>(postStat.st_atim), checkoutStart);
  EXPECT_GE(
      folly::to<system_clock::time_point>(postStat.st_mtim), checkoutStart);
  EXPECT_GE(
      folly::to<system_clock::time_point>(postStat.st_ctim), checkoutStart);
  if (preStat.hasValue()) {
    EXPECT_GE(
        folly::to<system_clock::time_point>(postStat.st_atim),
        folly::to<system_clock::time_point>(preStat->st_atim));
    EXPECT_GE(
        folly::to<system_clock::time_point>(postStat.st_mtim),
        folly::to<system_clock::time_point>(preStat->st_mtim));
    EXPECT_GE(
        folly::to<system_clock::time_point>(postStat.st_ctim),
        folly::to<system_clock::time_point>(preStat->st_ctim));
  }

  // Unmount and remount the mount point, and verify that the file changes
  // persisted across remount correctly.
  postInode.reset();
  testMount.remount();
  postInode = testMount.getFileInode(path);
  EXPECT_FILE_INODE(postInode, contents2, perms2);
}

void runModifyFileTests(folly::StringPiece path) {
  // Modify just the file contents, but not the permissions
  for (auto loadType : kAllLoadTypes) {
    SCOPED_TRACE(folly::to<string>(
        "contents change, path ", path, " load type ", loadType));
    testModifyFile(
        path,
        loadType,
        "contents v1",
        0644,
        "updated file contents\nextra stuff\n",
        0644);
  }

  // Modify just the permissions, but not the contents
  for (auto loadType : kAllLoadTypes) {
    SCOPED_TRACE(
        folly::to<string>("mode change, path ", path, " load type ", loadType));
    testModifyFile(path, loadType, "unchanged", 0755, "unchanged", 0644);
  }

  // Modify the contents and the permissions
  for (auto loadType : kAllLoadTypes) {
    SCOPED_TRACE(folly::to<string>(
        "contents+mode change, path ", path, " load type ", loadType));
    testModifyFile(
        path, loadType, "contents v1", 0644, "executable contents", 0755);
  }
}

TEST(Checkout, modifyFile) {
  // Test with file names that will be at the beginning of the directory,
  // in the middle of the directory, and at the end of the directory.
  runModifyFileTests("a/b/aaa.txt");
  runModifyFileTests("a/b/mmm.txt");
  runModifyFileTests("a/b/zzz.txt");
}

void testModifyConflict(
    folly::StringPiece path,
    LoadBehavior loadType,
    CheckoutMode checkoutMode,
    folly::StringPiece contents1,
    int perms1,
    folly::StringPiece currentContents,
    int currentPerms,
    folly::StringPiece contents2,
    int perms2) {
  // Prepare the tree to represent the current inode state
  auto workingDirBuilder = FakeTreeBuilder();
  workingDirBuilder.setFile("readme.txt", "just filling out the tree\n");
  workingDirBuilder.setFile("a/test.txt", "test contents\n");
  workingDirBuilder.setFile("a/b/dddd.c", "this is dddd.c\n");
  workingDirBuilder.setFile("a/b/tttt.c", "this is tttt.c\n");
  workingDirBuilder.setFile(path, currentContents, isExecutable(currentPerms));
  TestMount testMount{workingDirBuilder};

  // Prepare the "before" tree
  auto builder1 = workingDirBuilder.clone();
  builder1.replaceFile(path, contents1, isExecutable(perms1));
  builder1.finalize(testMount.getBackingStore(), true);
  // Reset the EdenMount to point at the tree from builder1, even though the
  // contents are still from workingDirBuilder.  This lets us trigger the
  // desired conflicts.
  //
  // TODO: We should also do a test where we start from builder1 then use
  // EdenDispatcher APIs to modify the contents to the "current" state.
  // This will have a different behavior than when using
  // resetCommit(), as the files will be materialized this way.
  auto commit1 = testMount.getBackingStore()->putCommit("a", builder1);
  commit1->setReady();
  testMount.getEdenMount()->resetParent(makeTestHash("a"));

  // Prepare the destination tree
  auto builder2 = builder1.clone();
  builder2.replaceFile(path, contents2, isExecutable(perms2));
  builder2.replaceFile("a/b/dddd.c", "new dddd contents\n");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("b", builder2);
  commit2->setReady();

  loadInodes(testMount, path, loadType, currentContents, currentPerms);

  auto checkoutResult =
      testMount.getEdenMount()->checkout(makeTestHash("b"), checkoutMode);
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  ASSERT_EQ(1, results.size());

  EXPECT_EQ(path, results[0].path);
  EXPECT_EQ(ConflictType::MODIFIED_MODIFIED, results[0].type);

  auto postInode = testMount.getFileInode(path);
  switch (checkoutMode) {
    case CheckoutMode::FORCE:
      // Make sure the path is updated as expected
      EXPECT_FILE_INODE(postInode, contents2, perms2);
      break;
    case CheckoutMode::DRY_RUN:
    case CheckoutMode::NORMAL:
      // Make sure the path has not been changed
      EXPECT_FILE_INODE(postInode, currentContents, currentPerms);
      break;
  }

  // Unmount and remount the mount point, and verify the changes persisted
  // across the remount as expected.
  postInode.reset();
  testMount.remount();
  postInode = testMount.getFileInode(path);
  auto ddddPath = "a/b/dddd.c";
  auto ddddInode = testMount.getFileInode(ddddPath);
  switch (checkoutMode) {
    case CheckoutMode::FORCE:
      EXPECT_FILE_INODE(postInode, contents2, perms2);
      EXPECT_FILE_INODE(ddddInode, "new dddd contents\n", 0644);
      break;
    case CheckoutMode::DRY_RUN:
      EXPECT_FILE_INODE(postInode, currentContents, currentPerms);
      EXPECT_FILE_INODE(ddddInode, "this is dddd.c\n", 0644);
      break;
    case CheckoutMode::NORMAL:
      EXPECT_FILE_INODE(postInode, currentContents, currentPerms);
      EXPECT_FILE_INODE(ddddInode, "new dddd contents\n", 0644);
      break;
  }
}

void runModifyConflictTests(CheckoutMode checkoutMode) {
  // Try with three separate path names, one that sorts first in the directory,
  // one in the middle, and one that sorts last.  This helps ensure that we
  // exercise all code paths in TreeInode::computeCheckoutActions()
  for (StringPiece path : {"a/b/aaa.txt", "a/b/mmm.txt", "a/b/zzz.tzt"}) {
    for (auto loadType : kAllLoadTypes) {
      SCOPED_TRACE(folly::to<string>(
          "path ", path, " load type ", loadType, " force=", checkoutMode));
      testModifyConflict(
          path,
          loadType,
          checkoutMode,
          "orig file contents.txt",
          0644,
          "current file contents.txt",
          0644,
          "new file contents.txt",
          0644);
    }
  }
}

TEST(Checkout, modifyConflictNormal) {
  runModifyConflictTests(CheckoutMode::NORMAL);
}

TEST(Checkout, modifyConflictDryRun) {
  runModifyConflictTests(CheckoutMode::DRY_RUN);
}

TEST(Checkout, modifyConflictForce) {
  runModifyConflictTests(CheckoutMode::FORCE);
}

TEST(Checkout, modifyThenRevert) {
  // Prepare a "before" tree
  auto srcBuilder = FakeTreeBuilder();
  srcBuilder.setFile("readme.txt", "just filling out the tree\n");
  srcBuilder.setFile("a/abc.txt", "foo\n");
  srcBuilder.setFile("a/test.txt", "test contents\n");
  srcBuilder.setFile("a/xyz.txt", "bar\n");
  TestMount testMount{srcBuilder};
  auto originalCommit = testMount.getEdenMount()->getParentCommits().parent1();

  // Modify a file.
  // We use the "normal" dispatcher APIs here, which will materialize the file.
  testMount.overwriteFile("a/test.txt", "temporary edit\n");

  auto preInode = testMount.getFileInode("a/test.txt");
  EXPECT_FILE_INODE(preInode, "temporary edit\n", 0644);

  // Now perform a forced checkout to the current commit,
  // which should discard our edits.
  auto checkoutResult =
      testMount.getEdenMount()->checkout(originalCommit, CheckoutMode::FORCE);
  ASSERT_TRUE(checkoutResult.isReady());
  // The checkout should report a/test.txt as a conflict
  EXPECT_THAT(
      checkoutResult.get(),
      UnorderedElementsAre(
          makeConflict(ConflictType::MODIFIED_MODIFIED, "a/test.txt")));

  // The checkout operation updates files by replacing them, so
  // there should be a new inode at this location now, with the original
  // contents.
  auto postInode = testMount.getFileInode("a/test.txt");
  EXPECT_FILE_INODE(postInode, "test contents\n", 0644);
  EXPECT_NE(preInode->getNodeId(), postInode->getNodeId());
  EXPECT_FILE_INODE(preInode, "temporary edit\n", 0644);
}

TEST(Checkout, modifyThenCheckoutRevisionWithoutFile) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "// Some code.\n");
  TestMount testMount{makeTestHash("1"), builder1};

  auto builder2 = builder1.clone();
  builder2.setFile("src/test.c", "// Unit test.\n");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  auto checkoutTo2 = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutTo2.isReady());

  testMount.overwriteFile("src/test.c", "temporary edit\n");
  auto checkoutTo1 = testMount.getEdenMount()->checkout(makeTestHash("1"));
  ASSERT_TRUE(checkoutTo1.isReady());

  EXPECT_THAT(
      checkoutTo1.get(),
      UnorderedElementsAre(
          makeConflict(ConflictType::MODIFIED_REMOVED, "src/test.c")));
}

TEST(Checkout, createUntrackedFileAndCheckoutAsTrackedFile) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "// Some code.\n");
  TestMount testMount{makeTestHash("1"), builder1};

  auto builder2 = builder1.clone();
  builder2.setFile("src/test.c", "// Unit test.\n");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  auto checkoutTo1 = testMount.getEdenMount()->checkout(makeTestHash("1"));
  ASSERT_TRUE(checkoutTo1.isReady());

  testMount.addFile("src/test.c", "temporary edit\n");
  auto checkoutTo2 = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutTo2.isReady());

  EXPECT_THAT(
      checkoutTo2.get(),
      UnorderedElementsAre(
          makeConflict(ConflictType::UNTRACKED_ADDED, "src/test.c")));
}

/*
 * This is similar to createUntrackedFileAndCheckoutAsTrackedFile, except it
 * exercises the case where the code must traverse into an untracked directory
 * and mark its contents UNTRACKED_ADDED, as appropriate.
 */
TEST(
    Checkout,
    createUntrackedFileAsOnlyDirectoryEntryAndCheckoutAsTrackedFile) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "// Some code.\n");
  TestMount testMount{makeTestHash("1"), builder1};

  auto builder2 = builder1.clone();
  builder2.setFile("src/test/test.c", "// Unit test.\n");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  auto checkoutTo1 = testMount.getEdenMount()->checkout(makeTestHash("1"));
  ASSERT_TRUE(checkoutTo1.isReady());

  testMount.mkdir("src/test");
  testMount.addFile("src/test/test.c", "temporary edit\n");
  auto checkoutTo2 = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutTo2.isReady());

  EXPECT_THAT(
      checkoutTo2.get(),
      UnorderedElementsAre(
          makeConflict(ConflictType::UNTRACKED_ADDED, "src/test/test.c")));
}

void testAddSubdirectory(folly::StringPiece newDirPath, LoadBehavior loadType) {
  auto builder1 = FakeTreeBuilder();
  builder1.setFile("src/main.c", "int main() { return 0; }\n");
  builder1.setFile("src/test/test.c", "testy tests");
  TestMount testMount{builder1};

  // Prepare a second tree, by starting with builder1 then adding
  // the new directory
  auto builder2 = builder1.clone();
  RelativePathPiece newDir{newDirPath};
  builder2.setFile(newDir + PathComponentPiece{"doc.txt"}, "docs\n");
  builder2.setFile(newDir + PathComponentPiece{"file1.c"}, "src\n");
  builder2.setFile(newDir + RelativePathPiece{"include/file1.h"}, "header\n");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  loadInodes(testMount, newDirPath, loadType);

  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  // Confirm that the tree has been updated correctly.
  EXPECT_FILE_INODE(
      testMount.getFileInode(newDir + PathComponentPiece{"doc.txt"}),
      "docs\n",
      0644);
  EXPECT_FILE_INODE(
      testMount.getFileInode(newDir + PathComponentPiece{"file1.c"}),
      "src\n",
      0644);
  EXPECT_FILE_INODE(
      testMount.getFileInode(newDir + RelativePathPiece{"include/file1.h"}),
      "header\n",
      0644);
}

TEST(Checkout, addSubdirectory) {
  // Test with multiple paths to exercise the case where the modification is at
  // the start of the directory listing, at the end, and in the middle.
  for (const auto& path : {"src/aaa", "src/ppp", "src/zzz"}) {
    for (auto loadType : kAddLoadTypes) {
      SCOPED_TRACE(folly::to<string>("path ", path, " load type ", loadType));
      testAddSubdirectory(path, loadType);
    }
  }
}

void testRemoveSubdirectory(LoadBehavior loadType) {
  // Build the destination source control tree first
  auto destBuilder = FakeTreeBuilder();
  destBuilder.setFile("src/main.c", "int main() { return 0; }\n");
  destBuilder.setFile("src/test/test.c", "testy tests");

  // Prepare the soruce tree by adding a new subdirectory (which will be
  // removed when we checkout from the src to the dest tree).
  auto srcBuilder = destBuilder.clone();
  RelativePathPiece path{"src/todelete"};
  srcBuilder.setFile(path + PathComponentPiece{"doc.txt"}, "docs\n");
  srcBuilder.setFile(path + PathComponentPiece{"file1.c"}, "src\n");
  srcBuilder.setFile(path + RelativePathPiece{"include/file1.h"}, "header\n");

  TestMount testMount{srcBuilder};
  destBuilder.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", destBuilder);
  commit2->setReady();

  loadInodes(testMount, path, loadType);

  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));
  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  // Confirm that the tree no longer exists.
  // None of the files should exist.
  EXPECT_THROW_ERRNO(
      testMount.getFileInode(path + PathComponentPiece{"doc.txt"}), ENOENT);
  EXPECT_THROW_ERRNO(
      testMount.getFileInode(path + PathComponentPiece{"file1.c"}), ENOENT);
  EXPECT_THROW_ERRNO(
      testMount.getFileInode(path + RelativePathPiece{"include/file1.h"}),
      ENOENT);
  // The two directories should have been removed too
  EXPECT_THROW_ERRNO(
      testMount.getTreeInode(path + RelativePathPiece{"include"}), ENOENT);
  EXPECT_THROW_ERRNO(testMount.getTreeInode(path), ENOENT);
}

// Remove a subdirectory with no conflicts or untracked files left behind
TEST(Checkout, removeSubdirectorySimple) {
  for (auto loadType : kAllLoadTypes) {
    SCOPED_TRACE(folly::to<string>(" load type ", loadType));
    testRemoveSubdirectory(loadType);
  }
}

TEST(Checkout, checkoutModifiesDirectoryDuringLoad) {
  auto builder1 = FakeTreeBuilder{};
  builder1.setFile("dir/sub/file.txt", "contents");
  TestMount testMount{builder1, false};
  builder1.setReady("");
  builder1.setReady("dir");

  // Prepare a second commit, pointing dir/sub to a different tree.
  auto builder2 = FakeTreeBuilder{};
  builder2.setFile("dir/sub/differentfile.txt", "differentcontents");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  // Begin loading "dir/sub".
  auto inodeFuture =
      testMount.getEdenMount()->getInode(RelativePathPiece{"dir/sub"});
  EXPECT_FALSE(inodeFuture.isReady());

  // Checkout to a revision where the contents of "dir/sub" have changed.
  auto checkoutResult = testMount.getEdenMount()->checkout(makeTestHash("2"));

  // The checkout ought to wait until the load completes.
  EXPECT_FALSE(checkoutResult.isReady());

  // Finish loading.
  builder1.setReady("dir/sub");
  EXPECT_TRUE(inodeFuture.isReady());

  ASSERT_TRUE(checkoutResult.isReady());
  auto results = checkoutResult.get();
  EXPECT_EQ(0, results.size());

  auto inode = inodeFuture.get().asTreePtr();
  EXPECT_EQ(
      0,
      inode->getContents().rlock()->entries.count(
          PathComponentPiece{"file.txt"}));
  EXPECT_EQ(
      1,
      inode->getContents().rlock()->entries.count(
          PathComponentPiece{"differentfile.txt"}));
}

TEST(Checkout, checkoutRemovingDirectoryDeletesOverlayFile) {
  auto builder1 = FakeTreeBuilder{};
  builder1.setFile("dir/sub/file.txt", "contents");
  TestMount testMount{builder1};

  // Prepare a second commit, removing dir/sub.
  auto builder2 = FakeTreeBuilder{};
  builder2.setFile("dir/tree/differentfile.txt", "differentcontents");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  // Load "dir/sub".
  auto subTree = testMount.getEdenMount()
                     ->getInode(RelativePathPiece{"dir/sub"})
                     .get(1ms)
                     .asTreePtr();
  auto subInodeNumber = subTree->getNodeId();
  subTree.reset();

  // Allocated inode numbers are saved during takeover.
  testMount.remountGracefully();

  EXPECT_TRUE(testMount.hasOverlayData(subInodeNumber));

  // Checkout to a revision without "dir/sub".
  auto checkoutResult =
      testMount.getEdenMount()->checkout(makeTestHash("2")).get(1ms);
  EXPECT_EQ(0, checkoutResult.size());

  // The checkout kicked off an async deletion of a subtree - wait for it to
  // complete.
  testMount.getEdenMount()->getOverlay()->flushPendingAsync().get(60s);

  EXPECT_FALSE(testMount.hasOverlayData(subInodeNumber));
}

TEST(Checkout, checkoutUpdatesUnlinkedStatusForLoadedTrees) {
  // This test is designed to stress the logic in
  // TreeInode::processCheckoutEntry that decides whether it's necessary to load
  // a TreeInode in order to continue.  It tests that unlinked status is
  // properly updated for tree inodes that have are referenced after a takeover.

  auto builder1 = FakeTreeBuilder{};
  builder1.setFile("dir/sub/file.txt", "contents");
  TestMount testMount{builder1};

  // Prepare a second commit, removing dir/sub.
  auto builder2 = FakeTreeBuilder{};
  builder2.setFile("dir/tree/differentfile.txt", "differentcontents");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  // Load "dir/sub" on behalf of a FUSE connection.
  auto subTree = testMount.getEdenMount()
                     ->getInode(RelativePathPiece{"dir/sub"})
                     .get(1ms)
                     .asTreePtr();
  auto subInodeNumber = subTree->getNodeId();
  subTree->incFuseRefcount();
  auto treeHash = subTree->getContents().rlock()->treeHash;
  subTree.reset();

  testMount.remountGracefully();

  // Checkout to a revision without "dir/sub" even though it's still referenced
  // by FUSE.
  auto checkoutResult =
      testMount.getEdenMount()->checkout(makeTestHash("2")).get(1ms);
  EXPECT_EQ(0, checkoutResult.size());

  // Try to load the same tree by its inode number. This will fail if the
  // unlinked bit wasn't set correctly.
  subTree = testMount.getEdenMount()
                ->getInodeMap()
                ->lookupInode(subInodeNumber)
                .get(1ms)
                .asTreePtr();
  auto subTreeContents = subTree->getContents().rlock();
  EXPECT_TRUE(subTree->isUnlinked());
  // Unlinked inodes are considered materialized?
  EXPECT_TRUE(subTreeContents->isMaterialized());

  auto dirTree = testMount.getEdenMount()
                     ->getInode(RelativePathPiece{"dir"})
                     .get(1ms)
                     .asTreePtr();
  auto dirContents = dirTree->getContents().rlock();
  EXPECT_FALSE(dirContents->isMaterialized());
}

TEST(Checkout, checkoutRemembersInodeNumbersAfterCheckoutAndTakeover) {
  auto builder1 = FakeTreeBuilder{};
  builder1.setFile("dir/sub/file1.txt", "contents1");
  TestMount testMount{builder1};

  // Prepare a second commit, changing dir/sub.
  auto builder2 = FakeTreeBuilder{};
  builder2.setFile("dir/sub/file2.txt", "contents2");
  builder2.finalize(testMount.getBackingStore(), true);
  auto commit2 = testMount.getBackingStore()->putCommit("2", builder2);
  commit2->setReady();

  // Load "dir/sub" on behalf of a FUSE connection.
  auto subTree = testMount.getEdenMount()
                     ->getInode(RelativePathPiece{"dir/sub"})
                     .get(1ms)
                     .asTreePtr();
  auto dirInodeNumber = subTree->getParentRacy()->getNodeId();
  auto subInodeNumber = subTree->getNodeId();
  subTree->incFuseRefcount();
  subTree.reset();

  // Checkout to a revision with a new dir/sub tree.  The old data should be
  // removed from the overlay.
  auto checkoutResult =
      testMount.getEdenMount()->checkout(makeTestHash("2")).get(1ms);
  EXPECT_EQ(0, checkoutResult.size());

  testMount.remountGracefully();

  // Try to load the same tree by its inode number and verify its parents have
  // the same inode numbers.
  subTree = testMount.getEdenMount()
                ->getInodeMap()
                ->lookupInode(subInodeNumber)
                .get(1ms)
                .asTreePtr();
  EXPECT_EQ(dirInodeNumber, subTree->getParentRacy()->getNodeId());
  EXPECT_EQ(subInodeNumber, subTree->getNodeId());

  auto subTree2 = testMount.getEdenMount()
                      ->getInode(RelativePathPiece{"dir/sub"})
                      .get(1ms)
                      .asTreePtr();
  EXPECT_EQ(dirInodeNumber, subTree2->getParentRacy()->getNodeId());
  EXPECT_EQ(subInodeNumber, subTree2->getNodeId());

  testMount.getEdenMount()->getInodeMap()->decFuseRefcount(subInodeNumber);
  subTree.reset();
  subTree2.reset();

  subTree = testMount.getEdenMount()
                ->getInode(RelativePathPiece{"dir/sub"})
                .get(1ms)
                .asTreePtr();
  EXPECT_EQ(dirInodeNumber, subTree->getParentRacy()->getNodeId());
  EXPECT_EQ(subInodeNumber, subTree->getNodeId());
}

// TODO:
// - remove subdirectory
//   - with no untracked/ignored files, it should get removed entirely
//   - remove subdirectory with untracked files
// - add/modify/replace symlink
//
// - change file type:
//   regular -> directory
//   regular -> symlink
//   symlink -> regular
//   symlink -> directory
//   directory -> regular
//   - also with error due to untracked files in directory
//   directory -> symlink
//   - also with error due to untracked files in directory
//
// - conflict handling, with and without --clean
//   - modify file, with removed conflict
//   - modify file, with changed file type conflict
//   - modify file, with a parent directory replaced with a file/symlink
//   - add file, with untracked file/directory/symlink already there
//   - add file, with a parent directory replaced with a file/symlink
//   - remove file, with modify conflict
//   - remove file, with remove conflict
//   - remove file, with a parent directory replaced with a file/symlink
