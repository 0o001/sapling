/*
 *  Copyright (c) 2018-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/inodes/GlobNode.h"
#include <folly/Exception.h>
#include <folly/experimental/TestUtil.h>
#include <folly/test/TestUtils.h>
#include <gtest/gtest.h>
#include <utility>
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/testharness/FakeBackingStore.h"
#include "eden/fs/testharness/FakeTreeBuilder.h"
#include "eden/fs/testharness/TestChecks.h"
#include "eden/fs/testharness/TestMount.h"

using namespace facebook;
using namespace facebook::eden;

using GlobResult = GlobNode::GlobResult;

enum StartReady : bool {
  DeferReady = false,
  StartReady = true,
};

enum Prefetch : bool {
  NoPrefetch = false,
  PrefetchBlobs = true,
};

class GlobNodeTest : public ::testing::TestWithParam<
                         std::pair<enum StartReady, enum Prefetch>> {
 protected:
  void SetUp() override {
    // The file contents are coupled with AHash, BHash and WatHash below.
    builder_.setFiles({{"dir/a.txt", "a"},
                       {"dir/sub/b.txt", "b"},
                       {".watchmanconfig", "wat"}});
    mount_.initialize(builder_, /*startReady=*/GetParam().first);
    prefetchHashes_ = nullptr;
  }

  std::vector<GlobResult> doGlob(
      folly::StringPiece pattern,
      bool includeDotfiles) {
    GlobNode globRoot(/*includeDotfiles=*/includeDotfiles);
    globRoot.parse(pattern);

    if (shouldPrefetch()) {
      prefetchHashes_ =
          std::make_shared<GlobNode::PrefetchList::element_type>();
    }

    auto rootInode = mount_.getTreeInode(RelativePathPiece());
    auto objectStore = mount_.getEdenMount()->getObjectStore();
    auto future = globRoot.evaluate(
        objectStore, RelativePathPiece(), rootInode, prefetchHashes_);

    if (!GetParam().first) {
      builder_.setAllReady();
    }
    return std::move(future).get();
  }

  std::vector<GlobResult> doGlobIncludeDotFiles(folly::StringPiece pattern) {
    return doGlob(pattern, true);
  }

  std::vector<GlobResult> doGlobExcludeDotFiles(folly::StringPiece pattern) {
    return doGlob(pattern, false);
  }

  bool shouldPrefetch() const {
    return GetParam().second;
  }

  std::vector<Hash> getPrefetchHashes() const {
    return *prefetchHashes_->rlock();
  }

  TestMount mount_;
  FakeTreeBuilder builder_;
  GlobNode::PrefetchList prefetchHashes_;
};

TEST_P(GlobNodeTest, starTxt) {
  auto matches = doGlobIncludeDotFiles("*.txt");
  EXPECT_TRUE(matches.empty());
  if (shouldPrefetch()) {
    EXPECT_TRUE(getPrefetchHashes().empty());
  }
}

// hash of "a"
const Hash AHash{"86f7e437faa5a7fce15d1ddcb9eaeaea377667b8"};
// hash of "b"
const Hash BHash{"e9d71f5ee7c92d6dc9e92ffdad17b8bd49418f98"};
// hash of "wat"
const Hash WatHash{"a3bbe1a8f2f025b8b6c5b66937763bb2b9bebdf2"};

TEST_P(GlobNodeTest, matchFilesByExtensionRecursively) {
  auto matches = doGlobIncludeDotFiles("**/*.txt");

  std::vector<GlobResult> expect{
      GlobResult("dir/a.txt"_relpath, dtype_t::Regular),
      GlobResult("dir/sub/b.txt"_relpath, dtype_t::Regular),
  };
  EXPECT_EQ(expect, matches);

  if (shouldPrefetch()) {
    std::vector<Hash> expectHashes{AHash, BHash};
    EXPECT_EQ(expectHashes, getPrefetchHashes());
  }
}

TEST_P(GlobNodeTest, star) {
  auto matches = doGlobIncludeDotFiles("*");

  std::vector<GlobResult> expect{
      GlobResult(".eden"_relpath, dtype_t::Dir),
      GlobResult(".watchmanconfig"_relpath, dtype_t::Regular),
      GlobResult("dir"_relpath, dtype_t::Dir)};
  EXPECT_EQ(expect, matches);

  if (shouldPrefetch()) {
    std::vector<Hash> expectHashes{WatHash};
    EXPECT_EQ(expectHashes, getPrefetchHashes());
  }
}

TEST_P(GlobNodeTest, starExcludeDot) {
  auto matches = doGlobExcludeDotFiles("*");

  std::vector<GlobResult> expect{GlobResult("dir"_relpath, dtype_t::Dir)};
  EXPECT_EQ(expect, matches);
}

TEST_P(GlobNodeTest, recursiveTxtWithChanges) {
  // Ensure that we enumerate things from the overlay
  mount_.addFile("root.txt", "added\n");
  mount_.addSymlink("sym.txt", "root.txt");
  // The mode change doesn't directly impact the results, but
  // does cause us to materialize this entry.  We just want
  // to make sure that it continues to show up after the change.
  builder_.setReady("dir");
  builder_.setReady("dir/a.txt");
  mount_.chmod("dir/a.txt", 0777);

  auto matches = doGlobIncludeDotFiles("**/*.txt");

  std::vector<GlobResult> expect{
      GlobResult("root.txt"_relpath, dtype_t::Regular),
      GlobResult("sym.txt"_relpath, dtype_t::Symlink),
      GlobResult("dir/a.txt"_relpath, dtype_t::Regular),
      GlobResult("dir/sub/b.txt"_relpath, dtype_t::Regular),
  };
  EXPECT_EQ(expect, matches);

  if (shouldPrefetch()) {
    std::vector<Hash> expectHashes{
        // No root.txt, as it is in the overlay
        // No sym.txt, as it is in the overlay
        // No AHash as we chmod'd the file and thus materialized it
        BHash};
    EXPECT_EQ(expectHashes, getPrefetchHashes());
  }
}

const std::pair<enum StartReady, enum Prefetch> combinations[] = {
    {StartReady::StartReady, Prefetch::NoPrefetch},
    {StartReady::StartReady, Prefetch::PrefetchBlobs},
    {StartReady::DeferReady, Prefetch::NoPrefetch},
    {StartReady::DeferReady, Prefetch::PrefetchBlobs},
};

INSTANTIATE_TEST_CASE_P(Glob, GlobNodeTest, ::testing::ValuesIn(combinations));
