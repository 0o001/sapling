/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */

#include "eden/win/fs/mount/EdenMount.h"

#include "eden/fs/config/CheckoutConfig.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/git/GitIgnoreStack.h"
#include "eden/fs/store/ObjectStore.h"
#include "eden/fs/utils/Bug.h"
#include "eden/fs/utils/Clock.h"
#include "eden/fs/utils/UnboundedQueueExecutor.h"

#include <folly/logging/xlog.h>

namespace facebook {
namespace eden {

static constexpr folly::StringPiece kEdenStracePrefix = "eden.strace.";

static uint64_t generateLuid() {
  LUID luid;
  if (AllocateLocallyUniqueId(&luid)) {
    uint64_t id = luid.HighPart;
    return id << 32 | luid.LowPart;
  }
  throw std::system_error(
      GetLastError(), Win32ErrorCategory::get(), "Failed to generate the luid");
}

std::shared_ptr<EdenMount> EdenMount::create(
    std::unique_ptr<CheckoutConfig> config,
    std::shared_ptr<ObjectStore> objectStore,
    std::shared_ptr<ServerState> serverState) {
  return std::shared_ptr<EdenMount>(
      new EdenMount(
          std::move(config), std::move(objectStore), std::move(serverState)),
      EdenMountDeleter{});
}

EdenMount::EdenMount(
    std::unique_ptr<CheckoutConfig> config,
    std::shared_ptr<ObjectStore> objectStore,
    std::shared_ptr<ServerState> serverState)
    : config_{std::move(config)},
      serverState_{std::move(serverState)},
      objectStore_{std::move(objectStore)},
      straceLogger_{kEdenStracePrefix.str() + config_->getMountPath().value()},
      dispatcher_{this},
      fsChannel_{config_->getMountPath(), this},
      mountGeneration_{generateLuid()} {
  auto parents = std::make_shared<ParentCommits>(config_->getParentCommits());

  XLOGF(
      INFO,
      "Creating eden mount {} Parent Commit {}",
      getPath(),
      parents->parent1().toString());
  parentInfo_.wlock()->parents.setParents(*parents);
}

EdenMount::~EdenMount() {}

const AbsolutePath& EdenMount::getPath() const {
  return config_->getMountPath();
}

folly::Future<std::shared_ptr<const Tree>> EdenMount::getRootTreeFuture()
    const {
  auto commitHash = Hash{parentInfo_.rlock()->parents.parent1()};
  return objectStore_->getTreeForCommit(commitHash);
}

std::shared_ptr<const Tree> EdenMount::getRootTree() const {
  // TODO: We should convert callers of this API to use the Future-based
  // version.
  return getRootTreeFuture().get();
}

void EdenMount::start() {
  fsChannel_.start();
}

void EdenMount::stop() {
  fsChannel_.stop();
}

void EdenMount::destroy() {
  XLOGF(
      INFO, "Destroying EdenMount (0x{:x})", reinterpret_cast<uintptr_t>(this));

  auto oldState = state_.exchange(State::DESTROYING);
  switch (oldState) {
    case State::RUNNING:
      stop();
      break;
  }
  delete this;
}

} // namespace eden
} // namespace facebook
