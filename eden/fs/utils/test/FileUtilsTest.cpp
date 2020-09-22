/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/utils/FileUtils.h"
#include <folly/Range.h>
#include <gtest/gtest.h>
#include <string>
#include "eden/fs/testharness/TempFile.h"
#include "eden/fs/utils/PathFuncs.h"

using namespace facebook::eden;
using folly::literals::string_piece_literals::operator""_sp;

namespace {
class FileUtilsTest : public ::testing::Test {
 protected:
  void SetUp() override {
    tempDir_ = makeTempDir();
    testLocation_ = AbsolutePath(tempDir_.path().native());
  }

  const AbsolutePathPiece getTestPath() {
    return testLocation_;
  }
  folly::test::TemporaryDirectory tempDir_;
  AbsolutePath testLocation_;
};
} // namespace

TEST_F(FileUtilsTest, testWriteReadFile) {
  auto filePath = getTestPath() + "testfile.txt"_pc;

  auto writtenContent = "This is the test file."_sp;

  writeFile(filePath, writtenContent).value();
  auto readContents = readFile(filePath).value();
  EXPECT_EQ(writtenContent, readContents);
}

TEST_F(FileUtilsTest, testReadPartialFile) {
  auto filePath = getTestPath() + "testfile.txt"_pc;
  auto writtenContent =
      "This is the test file. We plan to read the partial contents out of it"_sp;

  writeFile(filePath, writtenContent).value();
  std::string readContents = readFile(filePath, 10).value();
  EXPECT_EQ(writtenContent.subpiece(0, 10), readContents);
}

TEST_F(FileUtilsTest, testWriteFileAtomicNoTarget) {
  auto filePath = getTestPath() + "testfile.txt"_pc;
  auto writtenContent = "This is the test file."_sp;

  writeFileAtomic(filePath, writtenContent).value();
  std::string readContents = readFile(filePath).value();
  EXPECT_EQ(writtenContent, readContents);
}

TEST_F(FileUtilsTest, testWriteFileAtomicWithTarget) {
  auto filePath = getTestPath() + "testfile.txt"_pc;

  auto writtenContents1 = "This is the test file."_sp;
  auto writtenContents2 = "This is new contents."_sp;

  writeFile(filePath, writtenContents1).value();
  writeFileAtomic(filePath, writtenContents2).value();

  std::string readContents = readFile(filePath).value();
  EXPECT_EQ(writtenContents2, readContents);
}

TEST_F(FileUtilsTest, testWriteFileTruncate) {
  auto filePath = getTestPath() + "testfile.txt"_pc;

  writeFile(filePath, "Hello"_sp).value();
  writeFile(filePath, "hi"_sp).value();
  std::string readContents = readFile(filePath).value();
  EXPECT_EQ("hi", readContents);
}
