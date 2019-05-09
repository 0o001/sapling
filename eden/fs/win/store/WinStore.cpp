/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */

#include "eden/fs/win/store/WinStore.h"
#include <folly/Format.h>
#include <folly/logging/xlog.h>
#include <cstring>
#include <shared_mutex>
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/service/EdenError.h"
#include "eden/fs/store/BlobMetadata.h"
#include "eden/fs/win/mount/EdenMount.h"
#include "eden/fs/win/utils/StringConv.h"

namespace facebook {
namespace eden {
using namespace std;
using namespace folly;

WinStore::WinStore(const EdenMount* mount) : mount_{mount} {
  XLOGF(
      INFO,
      "Creating WinStore mount(0x{:x}) root {} WinStore (0x{:x}))",
      int(mount),
      mount->getPath(),
      int(this));
}
WinStore ::~WinStore() {}

shared_ptr<const Tree> WinStore::getTree(
    const RelativePathPiece& relPath) const {
  auto tree = mount_->getRootTree();

  for (auto piece : relPath.components()) {
    auto entry = tree->getEntryPtr(piece);
    if (entry != nullptr && entry->isTree()) {
      tree = mount_->getObjectStore()->getTree(entry->getHash()).get();
    } else {
      return nullptr;
    }
  }
  return tree;
}

shared_ptr<const Tree> WinStore::getTree(const std::wstring& path) const {
  std::string edenPath = winToEdenPath(path);
  RelativePathPiece relPath{edenPath};
  return getTree(relPath);
}

bool WinStore::getAllEntries(
    const std::wstring& path,
    vector<FileMetadata>& entryList) const {
  shared_ptr<const Tree> tree = getTree(path);

  if (!tree) {
    return false;
  }

  const std::vector<TreeEntry>& treeEntries = tree->getTreeEntries();
  vector<Future<pair<BlobMetadata, size_t>>> futures;
  for (size_t i = 0; i < treeEntries.size(); i++) {
    size_t fileSize = 0;
    if (!treeEntries[i].isTree()) {
      const std::optional<uint64_t>& size = treeEntries[i].getSize();
      if (size.has_value()) {
        fileSize = size.value();
      } else {
        futures.emplace_back(mount_->getObjectStore()
                                 ->getBlobMetadata(treeEntries[i].getHash())
                                 .thenValue([index = i](BlobMetadata data) {
                                   return make_pair(data, index);
                                 }));
        continue;
      }
    }

    entryList.emplace_back(
        std::move(
            edenToWinName(treeEntries[i].getName().value().toStdString())),
        treeEntries[i].isTree(),
        fileSize);
  }

  auto results = folly::collectAllSemiFuture(std::move(futures)).get();
  for (auto& result : results) {
    //
    // If we are here it's for a file, so the second argument will be false.
    //
    entryList.emplace_back(
        std::move(edenToWinName(
            treeEntries[result->second].getName().value().toStdString())),
        false,
        result->first.size);
  }

  return true;
}

bool WinStore::getFileMetadata(
    const std::wstring& path,
    FileMetadata& fileMetadata) const {
  std::string edenPath = winToEdenPath(path);
  RelativePathPiece relPath{edenPath};
  RelativePathPiece parentPath = relPath.dirname();
  shared_ptr<const Tree> tree = getTree(parentPath);
  if (tree) {
    auto entry = tree->getEntryPtr(relPath.basename());
    if (entry) {
      fileMetadata.name = edenToWinName(entry->getName().value().toStdString());
      fileMetadata.isDirectory = entry->isTree();

      if (!fileMetadata.isDirectory) {
        const std::optional<uint64_t>& size = entry->getSize();
        if (size.has_value()) {
          fileMetadata.size = size.value();
        } else {
          BlobMetadata metaData =
              mount_->getObjectStore()->getBlobMetadata(entry->getHash()).get();
          fileMetadata.size = metaData.size;
        }
      }
      return true;
    }
  }

  return false;
}

std::shared_ptr<const Blob> WinStore::getBlob(const std::wstring& path) const {
  std::string edenPath = winToEdenPath(path);
  RelativePathPiece relPath(edenPath);
  RelativePathPiece parentPath = relPath.dirname();

  auto tree = getTree(parentPath);
  if (!tree) {
    return nullptr;
  }

  auto file = tree->getEntryPtr(relPath.basename());
  if ((!file) || (file->isTree())) {
    return nullptr;
  }

  return (mount_->getObjectStore()->getBlob(file->getHash()).get());
}

} // namespace eden
} // namespace facebook
