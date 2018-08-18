/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "JournalDelta.h"
#include <folly/logging/xlog.h>

namespace facebook {
namespace eden {

namespace {
folly::StringPiece eventCharacterizationFor(const PathChangeInfo& ci) {
  if (ci.existedBefore && !ci.existedAfter) {
    return "Removed";
  } else if (!ci.existedBefore && ci.existedAfter) {
    return "Created";
  } else if (ci.existedBefore && ci.existedAfter) {
    return "Changed";
  } else {
    return "Ghost";
  }
}
} // namespace

JournalDelta::JournalDelta(RelativePathPiece fileName, JournalDelta::Created)
    : changedFilesInOverlay{{fileName.copy(), PathChangeInfo{false, true}}} {}

JournalDelta::JournalDelta(RelativePathPiece fileName, JournalDelta::Removed)
    : changedFilesInOverlay{{fileName.copy(), PathChangeInfo{true, false}}} {}

JournalDelta::JournalDelta(RelativePathPiece fileName, JournalDelta::Changed)
    : changedFilesInOverlay{{fileName.copy(), PathChangeInfo{true, true}}} {}

JournalDelta::JournalDelta(
    RelativePathPiece oldName,
    RelativePathPiece newName,
    JournalDelta::Renamed)
    : changedFilesInOverlay{{oldName.copy(), PathChangeInfo{true, false}},
                            {newName.copy(), PathChangeInfo{false, true}}} {}

JournalDelta::JournalDelta(
    RelativePathPiece oldName,
    RelativePathPiece newName,
    JournalDelta::Replaced)
    : changedFilesInOverlay{{oldName.copy(), PathChangeInfo{true, false}},
                            {newName.copy(), PathChangeInfo{true, true}}} {}

JournalDelta::~JournalDelta() {
  // O(1) stack space destruction of the delta chain.
  JournalDeltaPtr p{std::move(previous)};
  while (p && p.unique()) {
    // We know we have the only reference to p, so cast away constness because
    // we need to unset p->previous.
    JournalDelta* q = const_cast<JournalDelta*>(p.get());
    p = std::move(q->previous);
  }
}

std::unique_ptr<JournalDelta> JournalDelta::merge(
    SequenceNumber limitSequence,
    bool pruneAfterLimit) const {
  if (toSequence < limitSequence) {
    return nullptr;
  }

  const JournalDelta* current = this;

  auto result = std::make_unique<JournalDelta>();

  result->toSequence = current->toSequence;
  result->toTime = current->toTime;
  result->fromHash = fromHash;
  result->toHash = toHash;

  while (current) {
    if (current->toSequence < limitSequence) {
      break;
    }

    // Capture the lower bound.
    result->fromSequence = current->fromSequence;
    result->fromTime = current->fromTime;
    result->fromHash = current->fromHash;

    // Merge the unclean status list
    result->uncleanPaths.insert(
        current->uncleanPaths.begin(), current->uncleanPaths.end());

    for (auto& entry : current->changedFilesInOverlay) {
      auto& name = entry.first;
      auto& currentInfo = entry.second;
      auto* resultInfo = folly::get_ptr(result->changedFilesInOverlay, name);
      if (!resultInfo) {
        result->changedFilesInOverlay.emplace(name, currentInfo);
      } else {
        if (resultInfo->existedBefore != currentInfo.existedAfter) {
          auto event1 = eventCharacterizationFor(currentInfo);
          auto event2 = eventCharacterizationFor(*resultInfo);
          XLOG(ERR) << "Journal for " << name << " holds invalid " << event1
                    << ", " << event2 << " sequence";
        }

        resultInfo->existedBefore = currentInfo.existedBefore;
      }
    }

    // Continue the chain, but not if the caller requested that
    // we prune it out.
    if (!pruneAfterLimit) {
      result->previous = current->previous;
    }

    current = current->previous.get();
  }

  return result;
}

void JournalDelta::incRef() const noexcept {
  refCount_.fetch_add(1, std::memory_order_relaxed);
}

void JournalDelta::decRef() const noexcept {
  if (1 == refCount_.fetch_sub(1, std::memory_order_acq_rel)) {
    delete this;
  }
}

bool JournalDelta::isUnique() const noexcept {
  return 1 == refCount_.load(std::memory_order_acquire);
}

} // namespace eden
} // namespace facebook
