/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "EdenMountHandler.h"

#include <boost/polymorphic_cast.hpp>
#include <folly/Range.h>
#include "eden/fs/inodes/EdenMount.h"
#include "eden/fs/inodes/FileInode.h"
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/service/gen-cpp2/eden_types.h"
#include "eden/fuse/MountPoint.h"
#include "eden/fuse/fuse_headers.h"
#include "eden/utils/PathFuncs.h"

using folly::StringPiece;
using std::unique_ptr;

namespace facebook {
namespace eden {

void getMaterializedEntriesRecursive(
    std::map<std::string, FileInformation>& out,
    RelativePathPiece dirPath,
    TreeInode* dir);

void getMaterializedEntriesForMount(
    EdenMount* edenMount,
    MaterializedResult& out) {
  auto latest = edenMount->getJournal().rlock()->getLatest();

  out.currentPosition.mountGeneration = edenMount->getMountGeneration();
  out.currentPosition.sequenceNumber = latest->toSequence;
  out.currentPosition.snapshotHash =
      StringPiece(latest->toHash.getBytes()).str();

  auto rootInode = edenMount->getRootInode();
  if (rootInode) {
    getMaterializedEntriesRecursive(
        out.fileInfo, RelativePathPiece(), rootInode.get());
  }
}

// Convert from a system timespec to our thrift TimeSpec
static inline void timespecToTimeSpec(const timespec& src, TimeSpec& dest) {
  dest.seconds = src.tv_sec;
  dest.nanoSeconds = src.tv_nsec;
}

void getMaterializedEntriesRecursive(
    std::map<std::string, FileInformation>& out,
    RelativePathPiece dirPath,
    TreeInode* dir) {
  std::vector<std::pair<RelativePath, TreeInodePtr>> recurseList;
  {
    auto contents = dir->getContents().rlock();
    if (!contents->materialized) {
      return;
    }

    FileInformation dirInfo;
    auto attr = dir->getAttrLocked(&*contents);

    dirInfo.mode = attr.st.st_mode;
    timespecToTimeSpec(attr.st.st_mtim, dirInfo.mtime);

    out[dirPath.value().toString()] = std::move(dirInfo);

    for (auto& entIter : contents->entries) {
      const auto& name = entIter.first;
      const auto& ent = entIter.second;

      if (!ent->materialized) {
        continue;
      }

      // ent->inode is guaranteed to be set if ent->materialized is
      auto childInode = ent->inode;
      CHECK(childInode != nullptr);

      auto childPath = dirPath + name;
      if (S_ISDIR(ent->mode)) {
        auto childDir = boost::polymorphic_downcast<TreeInode*>(childInode);
        DCHECK(childDir->getContents().rlock()->materialized)
            << (dirPath + name) << " entry " << ent.get()
            << " materialized is true, but the contained dir is !materialized";
        recurseList.emplace_back(
            childPath, TreeInodePtr::newPtrLocked(childDir));
      } else {
        auto fileInode = boost::polymorphic_downcast<FileInode*>(childInode);
        auto attr = fileInode->getattr().get();

        FileInformation fileInfo;
        fileInfo.mode = attr.st.st_mode;
        fileInfo.size = attr.st.st_size;
        timespecToTimeSpec(attr.st.st_mtim, fileInfo.mtime);

        out[childPath.value().toStdString()] = std::move(fileInfo);
      }
    }
  }

  for (const auto& entry : recurseList) {
    getMaterializedEntriesRecursive(out, entry.first, entry.second.get());
  }
}
}
}
