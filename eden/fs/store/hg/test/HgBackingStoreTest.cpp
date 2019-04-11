/*
 *  Copyright (c) 2018-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/experimental/TestUtil.h>
#include <folly/test/TestUtils.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "eden/fs/model/Tree.h"
#include "eden/fs/store/MemoryLocalStore.h"
#include "eden/fs/store/ObjectStore.h"
#include "eden/fs/store/hg/HgBackingStore.h"
#include "eden/fs/store/hg/HgImporter.h"
#include "eden/fs/testharness/HgRepo.h"
#include "eden/fs/tracing/EdenStats.h"

using namespace facebook::eden;
using namespace std::chrono_literals;

struct TestRepo {
  folly::test::TemporaryDirectory testDir{"eden_hg_backing_store_test"};
  AbsolutePath testPath{testDir.path().string()};
  HgRepo repo{testPath + "repo"_pc};
  Hash commit1;

  TestRepo() {
    repo.hgInit();

    repo.mkdir("foo");
    repo.writeFile("foo/bar.txt", "bar\n");
    repo.mkdir("src");
    repo.writeFile("src/hello.txt", "world\n");
    repo.hg("add");
    commit1 = repo.commit("Initial commit");
  }
};

struct HgBackingStoreTest : TestRepo, ::testing::Test {
  HgBackingStoreTest() {}

  std::shared_ptr<MemoryLocalStore> localStore{
      std::make_shared<MemoryLocalStore>()};
  std::shared_ptr<ThreadLocalEdenStats> stats{
      std::make_shared<ThreadLocalEdenStats>()};
  HgImporter importer{repo.path(), localStore.get(), stats};
  std::shared_ptr<HgBackingStore> backingStore{
      std::make_shared<HgBackingStore>(&importer, localStore.get())};
  std::shared_ptr<ObjectStore> objectStore{
      ObjectStore::create(localStore, backingStore)};
};

TEST_F(
    HgBackingStoreTest,
    getTreeForCommit_reimports_tree_if_it_was_deleted_after_import) {
  auto tree1 = objectStore->getTreeForCommit(commit1).get(0ms);
  EXPECT_TRUE(tree1);
  ASSERT_THAT(
      tree1->getEntryNames(),
      ::testing::ElementsAre(PathComponent{"foo"}, PathComponent{"src"}));

  localStore->clearKeySpace(LocalStore::TreeFamily);
  auto tree2 = objectStore->getTreeForCommit(commit1).get(0ms);
  EXPECT_TRUE(tree2);
  ASSERT_THAT(
      tree1->getEntryNames(),
      ::testing::ElementsAre(PathComponent{"foo"}, PathComponent{"src"}));
}
