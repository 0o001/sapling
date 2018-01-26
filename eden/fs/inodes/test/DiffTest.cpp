/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/ExceptionWrapper.h>
#include <folly/experimental/logging/xlog.h>
#include <folly/test/TestUtils.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "eden/fs/inodes/DiffContext.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/InodeDiffCallback.h"
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/testharness/FakeBackingStore.h"
#include "eden/fs/testharness/FakeTreeBuilder.h"
#include "eden/fs/testharness/StoredObject.h"
#include "eden/fs/testharness/TestChecks.h"
#include "eden/fs/testharness/TestMount.h"
#include "eden/fs/testharness/TestUtil.h"

using namespace facebook::eden;
using folly::StringPiece;
using std::string;
using ::testing::UnorderedElementsAre;

class DiffResults {
 public:
  std::vector<RelativePath>& getUntracked() {
    return untracked_;
  }
  const std::vector<RelativePath>& getUntracked() const {
    return untracked_;
  }

  std::vector<RelativePath>& getIgnored() {
    return ignored_;
  }
  const std::vector<RelativePath>& getIgnored() const {
    return ignored_;
  }

  std::vector<RelativePath>& getRemoved() {
    return removed_;
  }
  const std::vector<RelativePath>& getRemoved() const {
    return removed_;
  }

  std::vector<RelativePath>& getModified() {
    return modified_;
  }
  const std::vector<RelativePath>& getModified() const {
    return modified_;
  }

  std::vector<std::pair<RelativePath, std::string>>& getErrors() {
    return errors_;
  }
  const std::vector<std::pair<RelativePath, std::string>>& getErrors() const {
    return errors_;
  }

 private:
  std::vector<RelativePath> untracked_;
  std::vector<RelativePath> ignored_;
  std::vector<RelativePath> removed_;
  std::vector<RelativePath> modified_;
  std::vector<std::pair<RelativePath, std::string>> errors_;
};

class DiffResultsCallback : public InodeDiffCallback {
 public:
  void ignoredFile(RelativePathPiece path) override {
    results_.wlock()->getIgnored().emplace_back(path);
  }
  void untrackedFile(RelativePathPiece path) override {
    results_.wlock()->getUntracked().emplace_back(path);
  }
  void removedFile(
      RelativePathPiece path,
      const TreeEntry& /* sourceControlEntry */) override {
    results_.wlock()->getRemoved().emplace_back(path);
  }
  void modifiedFile(
      RelativePathPiece path,
      const TreeEntry& /* sourceControlEntry */) override {
    results_.wlock()->getModified().emplace_back(path);
  }
  void diffError(RelativePathPiece path, const folly::exception_wrapper& ew)
      override {
    results_.wlock()->getErrors().emplace_back(
        RelativePath{path}, folly::exceptionStr(ew).toStdString());
  }

  /**
   * Extract the DiffResults object from this callback.
   *
   * This method should be called no more than once, as this destructively
   * moves the results out of the callback.  It should only be invoked after
   * the diff operation has completed.
   */
  DiffResults extractResults() {
    return std::move(*results_.wlock());
  }

 private:
  folly::Synchronized<DiffResults> results_;
};

template <typename T>
T getFutureResult(folly::Future<T>& future, const char* filename, int line) {
  if (!future.isReady()) {
    ADD_FAILURE_AT(filename, line) << "future not ready";
    throw folly::TimedOut();
  }
  if (future.hasException()) {
    ADD_FAILURE_AT(filename, line) << "future failed";
    // fall through and let get() throw.
  }
  return future.get();
}

#define EXPECT_FUTURE_RESULT(future) getFutureResult(future, __FILE__, __LINE__)

/**
 * A helper class for implementing the various diff tests.
 *
 * This is not implemented as a gtest fixture because using a standalone class
 * allows us to use multiple separate DiffTest objects in the same test case.
 * (This is mostly for convenience.  We could split things up into more test
 * cases if necessary, but defining so many separate TEST functions becomes
 * awkward.)
 */
class DiffTest {
 public:
  DiffTest() {
    // Set up a directory structure that we will use for most
    // of the tests below
    builder_.setFiles({
        {"src/1.txt", "This is src/1.txt.\n"},
        {"src/2.txt", "This is src/2.txt.\n"},
        {"src/a/b/3.txt", "This is 3.txt.\n"},
        {"src/a/b/c/4.txt", "This is 4.txt.\n"},
        {"doc/readme.txt", "No one reads docs.\n"},
        {"toplevel.txt", "toplevel\n"},
    });
    mount_.initialize(builder_);
  }

  explicit DiffTest(
      std::initializer_list<FakeTreeBuilder::FileInfo>&& fileArgs) {
    builder_.setFiles(std::move(fileArgs));
    mount_.initialize(builder_);
  }

  DiffResults diff(
      bool listIgnored = false,
      folly::StringPiece systemWideIgnoreFileContents = "",
      folly::StringPiece userIgnoreFileContents = "") {
    DiffResultsCallback callback;
    DiffContext diffContext{&callback,
                            listIgnored,
                            mount_.getEdenMount()->getObjectStore(),
                            systemWideIgnoreFileContents,
                            userIgnoreFileContents};
    auto diffFuture = mount_.getEdenMount()->diff(&diffContext);
    EXPECT_FUTURE_RESULT(diffFuture);
    return callback.extractResults();
  }
  folly::Future<DiffResults> diffFuture(bool listIgnored = false) {
    auto callback = std::make_unique<DiffResultsCallback>();
    auto diffFuture = mount_.getEdenMount()->diff(callback.get(), listIgnored);
    return diffFuture.then([callback = std::move(callback)]() {
      return callback->extractResults();
    });
  }

  DiffResults resetCommitAndDiff(FakeTreeBuilder& builder, bool loadInodes);

  void checkNoChanges() {
    auto result = diff();
    EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
    EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
    EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
    EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
    EXPECT_THAT(result.getModified(), UnorderedElementsAre());
  }

  void testResetFileModified(bool loadInodes);

  FakeTreeBuilder& getBuilder() {
    return builder_;
  }
  TestMount& getMount() {
    return mount_;
  }

 private:
  FakeTreeBuilder builder_;
  TestMount mount_;
};

/**
 * This method performs several steps:
 *
 * - Finalizes the supplied FakeTreeBuilder
 * - Creates a new commit from the resulting tree
 * - Calls EdenMount::resetCommit() to reset the current snapshot to point to
 *   this commit.  (This leaves the working directory unchanged, and only
 *   updates the current commit ID.)
 * - Calls EdenMount::diff(), waits for it to complete, and returns the
 *   results.
 */
DiffResults DiffTest::resetCommitAndDiff(
    FakeTreeBuilder& builder,
    bool loadInodes) {
  if (loadInodes) {
    mount_.loadAllInodes();
  }
  mount_.resetCommit(builder, /* setReady = */ true);
  auto df = diffFuture();
  return EXPECT_FUTURE_RESULT(df);
}

TEST(DiffTest, noChanges) {
  DiffTest test;
  // Run diff with no inodes loaded
  test.checkNoChanges();

  // Load all inodes then re-run the diff
  test.getMount().loadAllInodes();
  test.checkNoChanges();

  // Write the original contents to a file, and make sure it
  // still does not show up as changed.
  test.getMount().overwriteFile("src/1.txt", "This is src/1.txt.\n");
  test.checkNoChanges();
}

TEST(DiffTest, fileModified) {
  DiffTest test;
  test.getMount().overwriteFile("src/1.txt", "This file has been updated.\n");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
}

TEST(DiffTest, fileModeChanged) {
  DiffTest test;
  test.getMount().chmod("src/2.txt", 0755);

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"src/2.txt"}));
}

TEST(DiffTest, fileRemoved) {
  DiffTest test;
  test.getMount().deleteFile("src/1.txt");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, fileAdded) {
  DiffTest test;
  test.getMount().addFile("src/new.txt", "extra stuff");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/new.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, directoryRemoved) {
  DiffTest test;
  auto& mount = test.getMount();
  mount.deleteFile("src/a/b/3.txt");
  mount.deleteFile("src/a/b/c/4.txt");
  mount.rmdir("src/a/b/c");
  mount.rmdir("src/a/b");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(
          RelativePath{"src/a/b/3.txt"}, RelativePath{"src/a/b/c/4.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, directoryAdded) {
  DiffTest test;
  auto& mount = test.getMount();
  mount.mkdir("src/new");
  mount.mkdir("src/new/subdir");
  mount.addFile("src/new/file.txt", "extra stuff");
  mount.addFile("src/new/subdir/foo.txt", "extra stuff");
  mount.addFile("src/new/subdir/bar.txt", "more extra stuff");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"src/new/file.txt"},
          RelativePath{"src/new/subdir/foo.txt"},
          RelativePath{"src/new/subdir/bar.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, dirReplacedWithFile) {
  DiffTest test;
  auto& mount = test.getMount();
  mount.deleteFile("src/a/b/3.txt");
  mount.deleteFile("src/a/b/c/4.txt");
  mount.rmdir("src/a/b/c");
  mount.rmdir("src/a/b");
  mount.addFile("src/a/b", "this is now a file");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/a/b"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(
          RelativePath{"src/a/b/3.txt"}, RelativePath{"src/a/b/c/4.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, fileReplacedWithDir) {
  DiffTest test;
  auto& mount = test.getMount();
  mount.deleteFile("src/2.txt");
  mount.mkdir("src/2.txt");
  mount.mkdir("src/2.txt/subdir");
  mount.addFile("src/2.txt/file.txt", "extra stuff");
  mount.addFile("src/2.txt/subdir/foo.txt", "extra stuff");
  mount.addFile("src/2.txt/subdir/bar.txt", "more extra stuff");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"src/2.txt/file.txt"},
          RelativePath{"src/2.txt/subdir/foo.txt"},
          RelativePath{"src/2.txt/subdir/bar.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(), UnorderedElementsAre(RelativePath{"src/2.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

// Test file adds/removes/modifications with various orderings of names between
// the TreeInode entries and Tree entries.  This exercises the code that walks
// through the two entry lists comparing entry names.
TEST(DiffTest, pathOrdering) {
  DiffTest test({
      {"one/bbb.txt", "test\n"},
      {"one/xxx.txt", "test\n"},
      {"two/aaa.txt", "test\n"},
      {"two/bbb.txt", "test\n"},
      {"two/mmm.txt", "test\n"},
      {"two/xxx.txt", "test\n"},
      {"two/zzz.txt", "test\n"},
      {"three/aaa.txt", "test\n"},
      {"three/bbb.txt", "test\n"},
      {"three/mmm.txt", "test\n"},
      {"three/xxx.txt", "test\n"},
      {"three/zzz.txt", "test\n"},
  });
  auto& mount = test.getMount();

  // In directory one:
  // Add a file so that the TreeInode has the first entry, with no
  // corresponding entry in the source control tree.
  mount.addFile("one/aaa.txt", "test");
  // Add a file in the middle of the two entries in the source control Tree
  mount.addFile("one/mmm.txt", "test");
  // Add a file so that the TreeInode has the last entry, with no
  // corresponding entry in the source control tree.
  mount.addFile("one/zzz.txt", "test");

  // In directory two, remove the opposite entries, so that the source control
  // Tree has the first and last entries.
  mount.deleteFile("two/aaa.txt");
  mount.deleteFile("two/mmm.txt");
  mount.deleteFile("two/zzz.txt");

  // In directory three, overwrite these 3 entries, so that the first and last
  // files are modified, plus one in the middle.
  mount.overwriteFile("three/aaa.txt", "updated contents\n");
  mount.overwriteFile("three/mmm.txt", "updated contents\n");
  mount.overwriteFile("three/zzz.txt", "updated contents\n");

  // Perform the diff
  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"one/aaa.txt"},
          RelativePath{"one/mmm.txt"},
          RelativePath{"one/zzz.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(
          RelativePath{"two/aaa.txt"},
          RelativePath{"two/mmm.txt"},
          RelativePath{"two/zzz.txt"}));
  EXPECT_THAT(
      result.getModified(),
      UnorderedElementsAre(
          RelativePath{"three/aaa.txt"},
          RelativePath{"three/mmm.txt"},
          RelativePath{"three/zzz.txt"}));
}

/*
 * The following tests modify the directory contents using resetCommit()
 * This exercises a different code path than when using FUSE-like filesystem
 * APIs.  When using the normal filesystem APIs we end up with materialized
 * files.  When using resetCommit() we end up with files that are not
 * materialized, but are nonetheless different than the current commit.
 */

void testResetFileModified(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  auto b2 = t.getBuilder().clone();
  b2.replaceFile("src/1.txt", "This file has been updated.\n");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
}

TEST(DiffTest, resetFileModified) {
  testResetFileModified(true);
  testResetFileModified(false);
}

void testResetFileModeChanged(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  auto b2 = t.getBuilder().clone();
  b2.replaceFile("src/1.txt", "This is src/1.txt.\n", 0755);

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
}

TEST(DiffTest, resetFileModeChanged) {
  testResetFileModeChanged(true);
  testResetFileModeChanged(false);
}

void testResetFileRemoved(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with a new file added.
  // When we reset to it (without changing the working directory) it will look
  // like we have removed this file.
  auto b2 = t.getBuilder().clone();
  b2.setFile("src/notpresent.txt", "never present in the working directory");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(RelativePath{"src/notpresent.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetFileRemoved) {
  testResetFileRemoved(true);
  testResetFileRemoved(false);
}

void testResetFileAdded(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with a file removed.
  // When we reset to it (without changing the working directory) it will look
  // like we have added this file.
  auto b2 = t.getBuilder().clone();
  b2.removeFile("src/1.txt");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetFileAdded) {
  testResetFileAdded(true);
  testResetFileAdded(false);
}

void testResetDirectoryRemoved(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with a new directory added.
  // When we reset to it (without changing the working directory) it will look
  // like we have removed this directory.
  auto b2 = t.getBuilder().clone();
  b2.setFile("src/extradir/foo.txt", "foo");
  b2.setFile("src/extradir/bar.txt", "bar");
  b2.setFile("src/extradir/sub/1.txt", "1");
  b2.setFile("src/extradir/sub/xyz.txt", "xyz");
  b2.setFile("src/extradir/a/b/c/d/e.txt", "test");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(
          RelativePath{"src/extradir/foo.txt"},
          RelativePath{"src/extradir/bar.txt"},
          RelativePath{"src/extradir/sub/1.txt"},
          RelativePath{"src/extradir/sub/xyz.txt"},
          RelativePath{"src/extradir/a/b/c/d/e.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetDirectoryRemoved) {
  testResetDirectoryRemoved(true);
  testResetDirectoryRemoved(false);
}

void testResetDirectoryAdded(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with a directory removed.
  // When we reset to it (without changing the working directory) it will look
  // like we have added this directory.
  auto b2 = t.getBuilder().clone();
  b2.removeFile("src/a/b/3.txt");
  b2.removeFile("src/a/b/c/4.txt");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"src/a/b/3.txt"}, RelativePath{"src/a/b/c/4.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetDirectoryAdded) {
  testResetDirectoryAdded(true);
  testResetDirectoryAdded(false);
}

void testResetReplaceDirWithFile(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with 2.txt replaced by a directory added.
  // When we reset to it (without changing the working directory) it will look
  // like we have replaced this directory with the 2.txt file.
  auto b2 = t.getBuilder().clone();
  b2.removeFile("src/2.txt");
  b2.setFile("src/2.txt/foo.txt", "foo");
  b2.setFile("src/2.txt/bar.txt", "bar");
  b2.setFile("src/2.txt/sub/1.txt", "1");
  b2.setFile("src/2.txt/sub/xyz.txt", "xyz");
  b2.setFile("src/2.txt/a/b/c/d/e.txt", "test");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/2.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(
          RelativePath{"src/2.txt/foo.txt"},
          RelativePath{"src/2.txt/bar.txt"},
          RelativePath{"src/2.txt/sub/1.txt"},
          RelativePath{"src/2.txt/sub/xyz.txt"},
          RelativePath{"src/2.txt/a/b/c/d/e.txt"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetReplaceDirWithFile) {
  testResetReplaceDirWithFile(true);
  testResetReplaceDirWithFile(false);
}

void testResetReplaceFileWithDir(bool loadInodes) {
  SCOPED_TRACE(folly::to<string>("loadInodes=", loadInodes));

  DiffTest t;
  // Create a commit with a directory removed and replaced with a file.
  // When we reset to it (without changing the working directory) it will look
  // like we have removed the file and replaced it with the directory.
  auto b2 = t.getBuilder().clone();
  b2.removeFile("src/a/b/3.txt");
  b2.removeFile("src/a/b/c/4.txt");
  b2.replaceFile("src/a", "a is now a file");

  auto result = t.resetCommitAndDiff(b2, loadInodes);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"src/a/b/3.txt"}, RelativePath{"src/a/b/c/4.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre(RelativePath{"src/a"}));
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

TEST(DiffTest, resetReplaceFileWithDir) {
  testResetReplaceFileWithDir(true);
  testResetReplaceFileWithDir(false);
}

// Test with a .gitignore file in the top-level directory
TEST(DiffTest, ignoreToplevelOnly) {
  DiffTest test({
      {".gitignore", "/1.txt\nignore.txt\njunk/\n!important.txt\n"},
      {"a/b.txt", "test\n"},
      {"src/x.txt", "test\n"},
      {"src/y.txt", "test\n"},
      {"src/z.txt", "test\n"},
      {"src/foo/bar.txt", "test\n"},
  });

  // Add some untracked files, some of which match the ignore patterns
  test.getMount().addFile("1.txt", "new\n");
  test.getMount().addFile("ignore.txt", "new\n");
  test.getMount().addFile("src/1.txt", "new\n");
  test.getMount().addFile("src/foo/ignore.txt", "new\n");
  test.getMount().mkdir("src/foo/abc");
  test.getMount().mkdir("src/foo/abc/xyz");
  test.getMount().addFile("src/foo/abc/xyz/ignore.txt", "new\n");
  test.getMount().mkdir("junk");
  test.getMount().addFile("junk/stuff.txt", "new\n");
  // Even though important.txt matches an include rule, the fact that it
  // is inside an excluded directory takes precedence.
  test.getMount().addFile("junk/important.txt", "new\n");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(
          RelativePath{"1.txt"},
          RelativePath{"ignore.txt"},
          RelativePath{"junk/stuff.txt"},
          RelativePath{"junk/important.txt"},
          RelativePath{"src/foo/ignore.txt"},
          RelativePath{"src/foo/abc/xyz/ignore.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

// Test with a .gitignore file in the top-level directory
// and the presence of none, either, or both of system level
// and user specific ignore files
TEST(DiffTest, ignoreSystemLevelAndUser) {
  DiffTest test({
      {".gitignore", "/1.txt\nignore.txt\njunk/\n!important.txt\n"},
      {"a/b.txt", "test\n"},
      {"src/x.txt", "test\n"},
      {"src/y.txt", "test\n"},
      {"src/z.txt", "test\n"},
      {"src/foo/bar.txt", "test\n"},
  });

  // Add some untracked files, matching either global or user patterns
  test.getMount().addFile("skip_global.txt", "new\n");
  test.getMount().addFile("skip_user.txt", "new\n");

  auto result =
      test.diff(true /* listIgnored */, "skip_global.txt\n", "skip_user.txt\n");
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(
          RelativePath{"skip_global.txt"}, RelativePath{"skip_user.txt"}));

  result = test.diff(true /* listIgnored */, "", "skip_user.txt\n");
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getIgnored(), UnorderedElementsAre(RelativePath{"skip_user.txt"}));

  result = test.diff(true /* listIgnored */, "skip_global.txt\n", "");
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(RelativePath{"skip_global.txt"}));

  result = test.diff(true /* listIgnored */, "", "");
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
}

// test gitignore file which is a symlink
TEST(DiffTest, ignoreSymlink) {
  DiffTest test({
      {"actual", "/1.txt\nignore.txt\njunk/\n!important.txt\n"},
      {"a/b.txt", "test\n"},
      {"src/x.txt", "test\n"},
      {"src/y.txt", "test\n"},
      {"src/z.txt", "test\n"},
      {"src/foo/bar.txt", "test\n"},
  });
  test.getMount().addFile("1.txt", "new\n");
  test.getMount().addFile("ignore.txt", "new\n");

  test.getMount().addSymlink(".gitignore", "a/second");
  test.getMount().addSymlink("a/second", "../actual");
  test.getMount().addSymlink("a/.gitignore", ".gitignore");
  test.getMount().mkdir("b");
  test.getMount().addSymlink("b/.gitignore", "../b");
  test.getMount().addSymlink("src/.gitignore", "broken/link/to/nowhere");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(RelativePath{"1.txt"}, RelativePath{"ignore.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

// Test with a .gitignore file in the top-level directory
TEST(DiffTest, ignoreInSubdirectories) {
  DiffTest test({
      {".gitignore", "**/foo/bar.txt\n"},
      {"foo/.gitignore", "stuff\ntest\nwhatever\n"},
      {"foo/foo/.gitignore", "!/bar.txt\ntest\n"},
      {"abc/def/.gitignore", "*.log\n"},
      {"abc/def/other.txt", "test\n"},
      {"a/.gitignore", "b/c/d.txt\n"},
      {"a/b/c/x.txt", "test\n"},
      {"b/c/x.txt", "test\n"},
  });

  // Add some untracked files, some of which match the ignore patterns
  test.getMount().addFile("foo/bar.txt", "new\n");
  test.getMount().addFile("foo/foo/bar.txt", "new\n");
  test.getMount().mkdir("foo/test");
  test.getMount().addFile("foo/test/1.txt", "new\n");
  test.getMount().addFile("foo/test/2.txt", "new\n");
  test.getMount().mkdir("foo/test/3");
  test.getMount().addFile("foo/test/3/4.txt", "new\n");
  test.getMount().addFile("foo/foo/test", "new\n");
  test.getMount().addFile("test", "test\n");
  test.getMount().addFile("abc/def/test", "test\n");
  test.getMount().addFile("abc/def/test.log", "test\n");
  test.getMount().addFile("abc/def/another.log", "test\n");
  test.getMount().addFile("abc/test.log", "test\n");
  test.getMount().mkdir("abc/foo");
  test.getMount().addFile("abc/foo/bar.txt", "test\n");
  test.getMount().mkdir("other");
  test.getMount().addFile("other/bar.txt", "test\n");
  test.getMount().addFile("a/b/c/d.txt", "test\n");
  test.getMount().addFile("b/c/d.txt", "test\n");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"abc/test.log"},
          RelativePath{"abc/def/test"},
          RelativePath{"b/c/d.txt"},
          // Matches exlude rule in top-level .gitignore, but explicitly
          // included by "!bar.txt" rule in foo/foo/.gitignore
          RelativePath{"foo/foo/bar.txt"},
          RelativePath{"other/bar.txt"},
          RelativePath{"test"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"abc/test.log"},
          RelativePath{"abc/def/test"},
          RelativePath{"b/c/d.txt"},
          RelativePath{"foo/foo/bar.txt"},
          RelativePath{"other/bar.txt"},
          RelativePath{"test"}));
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(
          RelativePath{"a/b/c/d.txt"},
          // Ignored by "*.log" rule in abc/def/.gitignore
          RelativePath{"abc/def/test.log"},
          RelativePath{"abc/def/another.log"},
          // Ignored by "**/foo/bar.txt" rule in top-level .gitignore file
          RelativePath{"abc/foo/bar.txt"},
          // Ignored by "**/foo/bar.txt" rule in top-level .gitignore file
          RelativePath{"foo/bar.txt"},
          // Ignored by "test" rule in foo/.gitignore
          RelativePath{"foo/test/1.txt"},
          RelativePath{"foo/test/2.txt"},
          RelativePath{"foo/test/3/4.txt"},
          // Also ignored by "test" rule in foo/.gitignore
          RelativePath{"foo/foo/test"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

// Test when files already tracked in source control match ignore patterns
TEST(DiffTest, explicitlyTracked) {
  DiffTest test({
      {".gitignore", "1.txt\njunk\n"},
      {"junk/a/b/c.txt", "test\n"},
      {"junk/a/b/d.txt", "test\n"},
      {"junk/x/foo.txt", "test\n"},
      {"src/1.txt", "test\n"},
      {"docs/test.txt", "test\n"},
  });

  test.getMount().addFile("docs/1.txt", "new\n");
  test.getMount().addFile("junk/foo.txt", "new\n");
  test.getMount().addFile("junk/test.txt", "new\n");
  test.getMount().addFile("junk/a/b/xyz.txt", "new\n");
  test.getMount().addFile("other.txt", "new\n");
  test.getMount().overwriteFile("junk/a/b/c.txt", "new\n");
  test.getMount().deleteFile("junk/x/foo.txt");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"other.txt"}));
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(RelativePath{"junk/x/foo.txt"}));
  EXPECT_THAT(
      result.getModified(),
      UnorderedElementsAre(RelativePath{"junk/a/b/c.txt"}));

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(), UnorderedElementsAre(RelativePath{"other.txt"}));
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(
          RelativePath{"docs/1.txt"},
          RelativePath{"junk/foo.txt"},
          RelativePath{"junk/test.txt"},
          RelativePath{"junk/a/b/xyz.txt"}));
  EXPECT_THAT(
      result.getRemoved(),
      UnorderedElementsAre(RelativePath{"junk/x/foo.txt"}));
  EXPECT_THAT(
      result.getModified(),
      UnorderedElementsAre(RelativePath{"junk/a/b/c.txt"}));
}

// Test making modifications to the .gitignore file
TEST(DiffTest, ignoreFileModified) {
  DiffTest test({
      {"a/.gitignore", "foo.txt\n"},
  });

  test.getMount().addFile("a/foo.txt", "test\n");
  test.getMount().addFile("a/bar.txt", "test\n");
  test.getMount().addFile("a/test.txt", "test\n");

  auto result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"a/bar.txt"}, RelativePath{"a/test.txt"}));
  EXPECT_THAT(
      result.getIgnored(), UnorderedElementsAre(RelativePath{"a/foo.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());

  // Changes to the gitignore file should take effect immediately
  test.getMount().overwriteFile("a/.gitignore", "bar.txt\n");

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{"a/foo.txt"}, RelativePath{"a/test.txt"}));
  EXPECT_THAT(
      result.getIgnored(), UnorderedElementsAre(RelativePath{"a/bar.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"a/.gitignore"}));

  // Newly added gitignore files should also take effect immediately
  test.getMount().addFile(".gitignore", "test.txt\n");

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getUntracked(),
      UnorderedElementsAre(
          RelativePath{".gitignore"}, RelativePath{"a/foo.txt"}));
  EXPECT_THAT(
      result.getIgnored(),
      UnorderedElementsAre(
          RelativePath{"a/bar.txt"}, RelativePath{"a/test.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"a/.gitignore"}));
}

// Make sure the code ignores .gitignore directories
TEST(DiffTest, ignoreFileIsDirectory) {
  DiffTest test({
      {".gitignore", "1.txt\nignore.txt\n"},
      {"a/b.txt", "test\n"},
      {"a/.gitignore/b.txt", "test\n"},
      {"a/b/c.txt", "test\n"},
  });

  test.getMount().addFile("a/b/1.txt", "new\n");

  auto result = test.diff();
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());

  result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getIgnored(), UnorderedElementsAre(RelativePath{"a/b/1.txt"}));
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(result.getModified(), UnorderedElementsAre());
}

// Files under the .hg directory should never be reported in diff results
TEST(DiffTest, ignoreHidden) {
  DiffTest test({
      {"a/b.txt", "test\n"},
      {"a/c/d.txt", "test\n"},
      {"a/c/1.txt", "test\n"},
      {"a/c/2.txt", "test\n"},
  });

  test.getMount().mkdir(".hg");
  test.getMount().addFile(".hg/hgrc", "# hgrc contents would go here\n");
  test.getMount().addFile(".hg/bookmarks", "123456789 foobar\n");
  test.getMount().mkdir(".hg/store");
  test.getMount().mkdir(".hg/store/data");
  test.getMount().addFile(".hg/store/data/00changelog.d", "stuff\n");
  test.getMount().addFile(".hg/store/data/00changelog.i", "stuff\n");

  test.getMount().overwriteFile("a/c/1.txt", "updated contents\n");

  auto result = test.diff(true);
  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"a/c/1.txt"}));
}

TEST(DiffTest, diffRevisions) {
  DiffTest test;

  auto firstCommitHash = makeTestHash("a");
  auto secondCommitHash = makeTestHash("b");

  auto firstRoot = test.getBuilder().getRoot();
  auto firstCommit =
      test.getMount().getBackingStore()->putCommit(firstCommitHash, firstRoot);
  firstCommit->setReady();

  auto b2 = test.getBuilder().clone();
  b2.replaceFile("src/1.txt", "This file has been updated.\n");
  auto newRoot =
      b2.finalize(test.getMount().getBackingStore(), /*setReady=*/true);

  auto secondCommit =
      test.getMount().getBackingStore()->putCommit(secondCommitHash, newRoot);
  secondCommit->setReady();

  DiffResultsCallback callback;
  auto diffFuture = test.getMount().getEdenMount()->diffRevisions(
      &callback, firstCommitHash, secondCommitHash);
  EXPECT_FUTURE_RESULT(diffFuture);
  auto result = callback.extractResults();

  EXPECT_THAT(result.getErrors(), UnorderedElementsAre());
  EXPECT_THAT(result.getUntracked(), UnorderedElementsAre());
  EXPECT_THAT(result.getIgnored(), UnorderedElementsAre());
  EXPECT_THAT(result.getRemoved(), UnorderedElementsAre());
  EXPECT_THAT(
      result.getModified(), UnorderedElementsAre(RelativePath{"src/1.txt"}));
}
