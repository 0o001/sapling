/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/model/git/GitIgnoreStack.h"

namespace facebook {
namespace eden {

GitIgnore::MatchResult GitIgnoreStack::match(
    RelativePathPiece path,
    GitIgnore::FileType fileType) const {
  // Explicitly hide any entry named .hg or .eden
  //
  // We only check the very last component of the path.  Since these
  // directories are hidden the status code generally should not descend into
  // them and have to check ignore status for path names inside these
  // directories.
  const static PathComponentPiece kHgName{".hg"};
  const static PathComponentPiece kEdenName{".eden"};
  auto basename = path.basename();
  if (basename == kHgName || basename == kEdenName) {
    return GitIgnore::HIDDEN;
  }

  // Walk upwards through the GitIgnore stack, checking the path relative to
  // each directory against the GitIgnore rules for that directory.
  auto* node = this;
  auto suffixRange = path.rsuffixes();
  auto suffixIter = suffixRange.begin();
  while (node != nullptr) {
    RelativePathPiece suffix;
    if (suffixIter == suffixRange.end()) {
      // There may still be GitIgnore nodes to check even once we reach the
      // root directory.  The very first nodes in the ignore stack are used for
      // user-specific ignore rules, system-wide ignore rules, etc.
      //
      // All of these match against the full path from the mount point root.
      suffix = path;
    } else {
      suffix = *suffixIter;
      ++suffixIter;
    }

    const GitIgnore* ignore = &node->ignore_;
    node = node->parent_;

    auto result = ignore->match(suffix, basename, fileType);
    if (result != GitIgnore::NO_MATCH) {
      return result;
    }

    // We always expect to reach the end of the suffix iteration before
    // reaching the end of the GitIgnore file stack.
    //
    // We should add exactly one GitIgnore entry to the stack for each
    // directory.  We may also start with a few more GitIgnore entries on the
    // stack initially for system-wide or personal user ignore rules.
    DCHECK(node != nullptr || suffixIter == suffixRange.end());
  }
  return GitIgnore::NO_MATCH;
}
} // namespace eden
} // namespace facebook
