/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/Diff.h"

#include <folly/Portability.h>
#include <folly/Synchronized.h>
#include <folly/futures/Future.h>
#include <folly/logging/xlog.h>
#include <memory>
#include <vector>

#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/model/git/GitIgnoreStack.h"
#include "eden/fs/store/DiffContext.h"
#include "eden/fs/store/ObjectStore.h"
#include "eden/fs/store/ScmStatusDiffCallback.h"
#include "eden/fs/utils/Future.h"
#include "eden/fs/utils/PathFuncs.h"

using folly::Future;
using folly::makeFuture;
using folly::Try;
using folly::Unit;
using std::make_unique;
using std::vector;

namespace facebook::eden {

/*
 * In practice, while the functions in this file are comparing two source
 * control Tree objects, they are used for comparing the current
 * (non-materialized) working directory state (as wdTree) to its corresponding
 * source control state (as scmTree).
 */
namespace {

struct ChildFutures {
  void add(RelativePath&& path, Future<Unit>&& future) {
    paths.emplace_back(std::move(path));
    futures.emplace_back(std::move(future));
  }

  vector<RelativePath> paths;
  vector<Future<Unit>> futures;
};

struct DiffState {
  explicit DiffState(const ObjectStore* store)
      : callback{}, context{&callback, store} {}

  ScmStatusDiffCallback callback;
  DiffContext context;
};

static constexpr PathComponentPiece kIgnoreFilename{".gitignore"};

Future<Unit> diffAddedTree(
    DiffContext* context,
    RelativePathPiece entryPath,
    const Tree& wdTree,
    const GitIgnoreStack* ignore,
    bool isIgnored);

Future<Unit> diffRemovedTree(
    DiffContext* context,
    RelativePathPiece entryPath,
    const Tree& scmTree);

void processAddedSide(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& wdEntry,
    const GitIgnoreStack* ignore,
    bool isIgnored);

void processRemovedSide(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& scmEntry);

void processBothPresent(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& scmEntry,
    const TreeEntry& wdEntry,
    const GitIgnoreStack* ignore,
    bool isIgnored);

Future<Unit> waitOnResults(DiffContext* context, ChildFutures&& childFutures);

/**
 * Diff two trees.
 *
 * The path argument specifies the path to these trees, and will be prefixed
 * to all differences recorded in the results.
 *
 * The differences will be recorded using a callback provided by the caller.
 */
FOLLY_NODISCARD Future<Unit> computeTreeDiff(
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& scmTree,
    const Tree& wdTree,
    std::unique_ptr<GitIgnoreStack> ignore,
    bool isIgnored) {
  // A list of Futures to wait on for our children's results.
  ChildFutures childFutures;

  // Walk through the entries in both trees.
  // This relies on the fact that the entry list in each tree is always sorted.
  const auto& scmEntries = scmTree.getTreeEntries();
  const auto& wdEntries = wdTree.getTreeEntries();
  size_t scmIdx = 0;
  size_t wdIdx = 0;
  while (true) {
    if (scmIdx >= scmEntries.size()) {
      if (wdIdx >= wdEntries.size()) {
        // All Done
        break;
      }
      // This entry is present in wdTree but not scmTree
      processAddedSide(
          context,
          childFutures,
          currentPath,
          wdEntries[wdIdx],
          ignore.get(),
          isIgnored);
      ++wdIdx;
    } else if (wdIdx >= wdEntries.size()) {
      // This entry is present in scmTree but not wdTree
      processRemovedSide(
          context, childFutures, currentPath, scmEntries[scmIdx]);
      ++scmIdx;
    } else {
      auto compare = comparePathComponent(
          scmEntries[scmIdx].getName(),
          wdEntries[wdIdx].getName(),
          context->caseSensitive);
      if (compare == CompareResult::BEFORE) {
        processRemovedSide(
            context, childFutures, currentPath, scmEntries[scmIdx]);
        ++scmIdx;
      } else if (compare == CompareResult::AFTER) {
        processAddedSide(
            context,
            childFutures,
            currentPath,
            wdEntries[wdIdx],
            ignore.get(),
            isIgnored);
        ++wdIdx;
      } else {
        processBothPresent(
            context,
            childFutures,
            currentPath,
            scmEntries[scmIdx],
            wdEntries[wdIdx],
            ignore.get(),
            isIgnored);
        ++scmIdx;
        ++wdIdx;
      }
    }
  }

  // Add an ensure() block that makes sure the ignore stack exists until all of
  // our children results have finished processing
  return waitOnResults(context, std::move(childFutures))
      .ensure([ignore = std::move(ignore)] {});
}

FOLLY_NODISCARD Future<Unit> loadGitIgnoreThenDiffTrees(
    const TreeEntry& gitIgnoreEntry,
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& scmTree,
    const Tree& wdTree,
    const GitIgnoreStack* parentIgnore,
    bool isIgnored) {
  // TODO: load file contents directly from context->store if gitIgnoreEntry is
  // a regular file
  auto loadFileContentsFromPath = context->getLoadFileContentsFromPath();
  return loadFileContentsFromPath(
             context->getFetchContext(), currentPath + gitIgnoreEntry.getName())
      .thenError([entryPath = currentPath + gitIgnoreEntry.getName()](
                     const folly::exception_wrapper& ex) {
        // TODO: add an API to DiffCallback to report user errors like this
        // (errors that do not indicate a problem with EdenFS itself) that can
        // be returned to the caller in a thrift response
        XLOG(WARN) << "error loading gitignore at " << entryPath << ": "
                   << folly::exceptionStr(ex);
        return std::string{};
      })
      .thenValue([context,
                  currentPath = currentPath.copy(),
                  scmTree,
                  wdTree,
                  parentIgnore,
                  isIgnored](std::string&& ignoreFileContents) mutable {
        return computeTreeDiff(
            context,
            currentPath,
            scmTree,
            wdTree,
            make_unique<GitIgnoreStack>(parentIgnore, ignoreFileContents),
            isIgnored);
      });
}

FOLLY_NODISCARD Future<Unit> diffTrees(
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& scmTree,
    const Tree& wdTree,
    const GitIgnoreStack* parentIgnore,
    bool isIgnored) {
  if (context->isCancelled()) {
    XLOG(DBG7) << "diff() on directory " << currentPath
               << " cancelled due to client request no longer being active";
    return makeFuture();
  }
  // If this directory is already ignored, we don't need to bother loading its
  // .gitignore file.  Everything inside this directory must also be ignored,
  // unless it is explicitly tracked in source control.
  //
  // Explicit include rules cannot be used to unignore files inside an ignored
  // directory.
  //
  // We check context->getLoadFileContentsFromPath() here as a way to see if we
  // are processing gitIgnore files or not, since this is only set from code
  // that enters through eden/fs/inodes/Diff.cpp. Either way, it is
  // impossible to load file contents without this set.
  if (isIgnored || !context->getLoadFileContentsFromPath()) {
    // We can pass in a null GitIgnoreStack pointer here.
    // Since the entire directory is ignored, we don't need to check ignore
    // status for any entries that aren't already tracked in source control.
    return computeTreeDiff(
        context, currentPath, scmTree, wdTree, nullptr, isIgnored);
  }

  // If this directory has a .gitignore file, load it first.
  const auto* gitIgnoreEntry = wdTree.getEntryPtr(kIgnoreFilename);
  if (gitIgnoreEntry && !gitIgnoreEntry->isTree()) {
    return loadGitIgnoreThenDiffTrees(
        *gitIgnoreEntry,
        context,
        currentPath,
        scmTree,
        wdTree,
        parentIgnore,
        isIgnored);
  }

  return computeTreeDiff(
      context,
      currentPath,
      scmTree,
      wdTree,
      make_unique<GitIgnoreStack>(parentIgnore), // empty with no rules
      isIgnored);
}

FOLLY_NODISCARD Future<Unit> processAddedChildren(
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& wdTree,
    std::unique_ptr<GitIgnoreStack> ignore,
    bool isIgnored) {
  ChildFutures childFutures;
  for (const auto& childEntry : wdTree.getTreeEntries()) {
    processAddedSide(
        context,
        childFutures,
        currentPath,
        childEntry,
        ignore.get(),
        isIgnored);
  }

  // Add an ensure() block that makes sure the ignore stack exists until all of
  // our children results have finished processing
  return waitOnResults(context, std::move(childFutures))
      .ensure([ignore = std::move(ignore)] {});
}

FOLLY_NODISCARD Future<Unit> loadGitIgnoreThenProcessAddedChildren(
    const TreeEntry& gitIgnoreEntry,
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& wdTree,
    const GitIgnoreStack* parentIgnore,
    bool isIgnored) {
  auto loadFileContentsFromPath = context->getLoadFileContentsFromPath();
  return loadFileContentsFromPath(
             context->getFetchContext(), currentPath + gitIgnoreEntry.getName())
      .thenError([entryPath = currentPath + gitIgnoreEntry.getName()](
                     const folly::exception_wrapper& ex) {
        XLOG(WARN) << "error loading gitignore at " << entryPath << ": "
                   << folly::exceptionStr(ex);
        return std::string{};
      })
      .thenValue([context,
                  currentPath = currentPath.copy(),
                  wdTree,
                  parentIgnore,
                  isIgnored](std::string&& ignoreFileContents) mutable {
        return processAddedChildren(
            context,
            currentPath,
            wdTree,
            make_unique<GitIgnoreStack>(parentIgnore, ignoreFileContents),
            isIgnored);
      });
}

/**
 * Process a Tree that is present only on one side of the diff.
 */
FOLLY_NODISCARD Future<Unit> diffAddedTree(
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& wdTree,
    const GitIgnoreStack* parentIgnore,
    bool isIgnored) {
  if (context->isCancelled()) {
    XLOG(DBG7) << "diff() on directory " << currentPath
               << " cancelled due to client request no longer being active";
    return makeFuture();
  }
  ChildFutures childFutures;

  // If this directory is already ignored, we don't need to bother loading its
  // .gitignore file.  Everything inside this directory must also be ignored,
  // unless it is explicitly tracked in source control.
  //
  // Also, if we are not honoring gitignored files, then do not bother loading
  // its .gitignore file
  //
  // Explicit include rules cannot be used to unignore files inside an ignored
  // directory.
  //
  // We check context->getLoadFileContentsFromPath() here as a way to see if we
  // are processing gitIgnore files or not, since this is only set from code
  // that enters through eden/fs/inodes/DiffTree.cpp. Either way, it is
  // impossible to load file contents without this set.
  if (isIgnored || !context->getLoadFileContentsFromPath()) {
    // We can pass in a null GitIgnoreStack pointer here.
    // Since the entire directory is ignored, we don't need to check ignore
    // status for any entries that aren't already tracked in source control.
    return processAddedChildren(
        context, currentPath, wdTree, nullptr, isIgnored);
  }

  // If this directory has a .gitignore file, load it first.
  const auto* gitIgnoreEntry = wdTree.getEntryPtr(kIgnoreFilename);
  if (gitIgnoreEntry && !gitIgnoreEntry->isTree()) {
    return loadGitIgnoreThenProcessAddedChildren(
        *gitIgnoreEntry, context, currentPath, wdTree, parentIgnore, isIgnored);
  }

  return processAddedChildren(
      context,
      currentPath,
      wdTree,
      make_unique<GitIgnoreStack>(parentIgnore), // empty with no rules
      isIgnored);
}

/**
 * Process a Tree that is present only on one side of the diff.
 */
FOLLY_NODISCARD Future<Unit> diffRemovedTree(
    DiffContext* context,
    RelativePathPiece currentPath,
    const Tree& scmTree) {
  if (context->isCancelled()) {
    XLOG(DBG7) << "diff() on directory " << currentPath
               << " cancelled due to client request no longer being active";
    return makeFuture();
  }
  ChildFutures childFutures;
  for (const auto& childEntry : scmTree.getTreeEntries()) {
    processRemovedSide(context, childFutures, currentPath, childEntry);
  }
  return waitOnResults(context, std::move(childFutures));
}

/**
 * Process a TreeEntry that is present only on one side of the diff.
 * We don't know yet if this TreeEntry refers to a Tree or a Blob.
 *
 * If we could not compute a result immediately we will add an entry to
 * childFutures.
 */
void processRemovedSide(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& scmEntry) {
  if (!scmEntry.isTree()) {
    context->callback->removedFile(currentPath + scmEntry.getName());
    return;
  }
  auto entryPath = currentPath + scmEntry.getName();
  auto childFuture = diffRemovedTree(context, entryPath, scmEntry.getHash());
  childFutures.add(std::move(entryPath), std::move(childFuture));
}

/**
 * Process a TreeEntry that is present only on one side of the diff.
 * We don't know yet if this TreeEntry refers to a Tree or a Blob.
 *
 * If we could not compute a result immediately we will add an entry to
 * childFutures.
 */
void processAddedSide(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& wdEntry,
    const GitIgnoreStack* ignore,
    bool isIgnored) {
  bool entryIgnored = isIgnored;
  auto entryPath = currentPath + wdEntry.getName();
  if (!isIgnored && ignore) {
    auto fileType =
        wdEntry.isTree() ? GitIgnore::TYPE_DIR : GitIgnore::TYPE_FILE;
    auto ignoreStatus = ignore->match(entryPath, fileType);
    if (ignoreStatus == GitIgnore::HIDDEN) {
      // Completely skip over hidden entries.
      // This is used for reserved directories like .hg and .eden
      return;
    }
    entryIgnored = (ignoreStatus == GitIgnore::EXCLUDE);
  }

  if (wdEntry.isTree()) {
    if (!entryIgnored || context->listIgnored) {
      auto childFuture = diffAddedTree(
          context, entryPath, wdEntry.getHash(), ignore, entryIgnored);
      childFutures.add(std::move(entryPath), std::move(childFuture));
    }
  } else {
    if (!entryIgnored) {
      context->callback->addedFile(entryPath);
    } else if (context->listIgnored) {
      context->callback->ignoredFile(entryPath);
    } else {
      // Don't bother reporting this ignored file since
      // listIgnored is false.
    }
  }
}

/**
 * Process TreeEntry objects that exist on both sides of the diff.
 */
void processBothPresent(
    DiffContext* context,
    ChildFutures& childFutures,
    RelativePathPiece currentPath,
    const TreeEntry& scmEntry,
    const TreeEntry& wdEntry,
    const GitIgnoreStack* ignore,
    bool isIgnored) {
  bool entryIgnored = isIgnored;
  auto entryPath = currentPath + scmEntry.getName();
  // If wdEntry and scmEntry are both files (or symlinks) then we don't need
  // to bother computing the ignore status: the file is explicitly tracked in
  // source control, so we should report it's status even if it would normally
  // be ignored.
  if (!isIgnored && (wdEntry.isTree() || scmEntry.isTree()) && ignore) {
    auto fileType =
        wdEntry.isTree() ? GitIgnore::TYPE_DIR : GitIgnore::TYPE_FILE;
    auto ignoreStatus = ignore->match(entryPath, fileType);
    if (ignoreStatus == GitIgnore::HIDDEN) {
      // This is rather unexpected.  We don't expect to find entries in
      // source control using reserved hidden names.
      // Treat this as ignored for now.
      entryIgnored = true;
    } else if (ignoreStatus == GitIgnore::EXCLUDE) {
      entryIgnored = true;
    } else {
      entryIgnored = false;
    }
  }

  bool isTreeSCM = scmEntry.isTree();
  bool isTreeWD = wdEntry.isTree();

  if (isTreeSCM) {
    if (isTreeWD) {
      // tree-to-tree diff
      XDCHECK_EQ(scmEntry.getType(), wdEntry.getType());
      if (scmEntry.getHash() == wdEntry.getHash()) {
        return;
      }
      auto childFuture = diffTrees(
          context,
          entryPath,
          scmEntry.getHash(),
          wdEntry.getHash(),
          ignore,
          entryIgnored);
      childFutures.add(std::move(entryPath), std::move(childFuture));
    } else {
      // tree-to-file
      // Add a ADDED entry for this path
      if (entryIgnored) {
        if (context->listIgnored) {
          context->callback->ignoredFile(entryPath);
        }
      } else {
        context->callback->addedFile(entryPath);
      }

      // Report everything in scmTree as REMOVED
      auto childFuture =
          diffRemovedTree(context, entryPath, scmEntry.getHash());
      childFutures.add(std::move(entryPath), std::move(childFuture));
    }
  } else {
    if (isTreeWD) {
      // file-to-tree
      // Add a REMOVED entry for this path
      context->callback->removedFile(entryPath);

      // Report everything in wdEntry as ADDED
      auto childFuture = diffAddedTree(
          context, entryPath, wdEntry.getHash(), ignore, entryIgnored);
      childFutures.add(std::move(entryPath), std::move(childFuture));
    } else {
      // file-to-file diff
      // Even if blobs have different hashes, they could have the same contents.
      // For example, if between the two revisions being compared, if a file was
      // changed and then later reverted. In that case, the contents would be
      // the same but the blobs would have different hashes
      // If the types are different, then this entry is definitely modified
      if (scmEntry.getType() != wdEntry.getType()) {
        context->callback->modifiedFile(entryPath);
      } else {
        // If Mercurial eventually switches to using blob IDs that are solely
        // based on the file contents (as opposed to file contents + history)
        // then we could drop this extra load of the blob SHA-1, and rely only
        // on the blob ID comparison instead.
        auto compareEntryContents =
            folly::makeFutureWith([context,
                                   entryPath = currentPath + scmEntry.getName(),
                                   &scmEntry,
                                   &wdEntry] {
              auto scmFuture = context->store->getBlobSha1(
                  scmEntry.getHash(), context->getFetchContext());
              auto wdFuture = context->store->getBlobSha1(
                  wdEntry.getHash(), context->getFetchContext());
              return collectSafe(scmFuture, wdFuture)
                  .thenValue([entryPath = entryPath.copy(),
                              context](const std::tuple<Hash, Hash>& info) {
                    const auto& [scmHash, wdHash] = info;
                    if (scmHash != wdHash) {
                      context->callback->modifiedFile(entryPath);
                    }
                  });
            });
        childFutures.add(std::move(entryPath), std::move(compareEntryContents));
      }
    }
  }
}

FOLLY_NODISCARD Future<Unit> waitOnResults(
    DiffContext* context,
    ChildFutures&& childFutures) {
  XDCHECK_EQ(childFutures.paths.size(), childFutures.futures.size());
  if (childFutures.futures.empty()) {
    return makeFuture();
  }

  return folly::collectAll(std::move(childFutures.futures))
      .toUnsafeFuture()
      .thenValue([context, paths = std::move(childFutures.paths)](
                     vector<Try<Unit>>&& results) {
        XDCHECK_EQ(paths.size(), results.size());
        for (size_t idx = 0; idx < results.size(); ++idx) {
          const auto& result = results[idx];
          if (!result.hasException()) {
            continue;
          }
          XLOG(ERR) << "error computing SCM diff for " << paths.at(idx);
          context->callback->diffError(paths.at(idx), result.exception());
        }
      });
}

/**
 * Diff two commits.
 *
 * The differences will be recorded using a callback inside of DiffState and
 * will be extracted and returned to the caller.
 */
FOLLY_NODISCARD Future<Unit>
diffRoots(DiffContext* context, const RootId& root1, const RootId& root2) {
  auto future1 = context->store->getRootTree(root1, context->getFetchContext());
  auto future2 = context->store->getRootTree(root2, context->getFetchContext());
  return collectSafe(future1, future2)
      .thenValue([context](std::tuple<
                           std::shared_ptr<const Tree>,
                           std::shared_ptr<const Tree>>&& tup) {
        const auto& [tree1, tree2] = tup;

        // This happens in the case in which the CLI (during eden doctor) calls
        // getScmStatusBetweenRevisions() with the same hash in order to check
        // if a commit hash is valid.
        if (tree1->getHash() == tree2->getHash()) {
          return makeFuture();
        }

        return diffTrees(
            context, RelativePathPiece{}, *tree1, *tree2, nullptr, false);
      });
}
} // namespace

Future<std::unique_ptr<ScmStatus>> diffCommitsForStatus(
    const ObjectStore* store,
    const RootId& root1,
    const RootId& root2) {
  return folly::makeFutureWith([&] {
    auto state = std::make_unique<DiffState>(store);
    auto statePtr = state.get();
    auto contextPtr = &(statePtr->context);
    return diffRoots(contextPtr, root1, root2)
        .thenValue([state = std::move(state)](auto&&) {
          return std::make_unique<ScmStatus>(state->callback.extractStatus());
        });
  });
}

FOLLY_NODISCARD Future<Unit> diffTrees(
    DiffContext* context,
    RelativePathPiece currentPath,
    Hash scmHash,
    Hash wdHash,
    const GitIgnoreStack* ignore,
    bool isIgnored) {
  auto scmTreeFuture =
      context->store->getTree(scmHash, context->getFetchContext());
  auto wdTreeFuture =
      context->store->getTree(wdHash, context->getFetchContext());
  // Optimization for the case when both tree objects are immediately ready.
  // We can avoid copying the input path in this case.
  if (scmTreeFuture.isReady() && wdTreeFuture.isReady()) {
    return diffTrees(
        context,
        currentPath,
        *(std::move(scmTreeFuture).get()),
        *(std::move(wdTreeFuture).get()),
        ignore,
        isIgnored);
  }

  return collectSafe(scmTreeFuture, wdTreeFuture)
      .thenValue([context, currentPath = currentPath.copy(), ignore, isIgnored](
                     std::tuple<
                         std::shared_ptr<const Tree>,
                         std::shared_ptr<const Tree>>&& tup) {
        const auto& [scmTree, wdTree] = tup;
        return diffTrees(
            context, currentPath, *scmTree, *wdTree, ignore, isIgnored);
      });
}

FOLLY_NODISCARD Future<Unit> diffAddedTree(
    DiffContext* context,
    RelativePathPiece currentPath,
    Hash wdHash,
    const GitIgnoreStack* ignore,
    bool isIgnored) {
  auto wdFuture = context->store->getTree(wdHash, context->getFetchContext());
  // Optimization for the case when the tree object is immediately ready.
  // We can avoid copying the input path in this case.
  if (wdFuture.isReady()) {
    return diffAddedTree(
        context, currentPath, *std::move(wdFuture).get(), ignore, isIgnored);
  }

  return std::move(wdFuture).thenValue(
      [context, currentPath = currentPath.copy(), ignore, isIgnored](
          std::shared_ptr<const Tree>&& wdTree) {
        return diffAddedTree(context, currentPath, *wdTree, ignore, isIgnored);
      });
}

FOLLY_NODISCARD Future<Unit> diffRemovedTree(
    DiffContext* context,
    RelativePathPiece currentPath,
    Hash scmHash) {
  auto scmFuture = context->store->getTree(scmHash, context->getFetchContext());
  // Optimization for the case when the tree object is immediately ready.
  // We can avoid copying the input path in this case.
  if (scmFuture.isReady()) {
    return diffRemovedTree(context, currentPath, *(std::move(scmFuture).get()));
  }

  return std::move(scmFuture).thenValue(
      [context,
       currentPath = currentPath.copy()](std::shared_ptr<const Tree>&& tree) {
        return diffRemovedTree(context, currentPath, *tree);
      });
}

} // namespace facebook::eden
