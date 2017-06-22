/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "ObjectStore.h"

#include <folly/Conv.h>
#include <folly/Optional.h>
#include <folly/futures/Future.h>
#include <folly/io/IOBuf.h>
#include <stdexcept>
#include "BackingStore.h"
#include "LocalStore.h"
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Tree.h"

using folly::Future;
using folly::IOBuf;
using folly::makeFuture;
using std::shared_ptr;
using std::string;
using std::unique_ptr;

namespace facebook {
namespace eden {

ObjectStore::ObjectStore(
    shared_ptr<LocalStore> localStore,
    shared_ptr<BackingStore> backingStore)
    : localStore_(std::move(localStore)),
      backingStore_(std::move(backingStore)) {}

ObjectStore::~ObjectStore() {}

Future<unique_ptr<Tree>> ObjectStore::getTreeFuture(const Hash& id) const {
  // Check in the LocalStore first
  auto tree = localStore_->getTree(id);
  if (tree) {
    VLOG(4) << "tree " << id << " found in local store";
    return makeFuture(std::move(tree));
  }

  // Note: We don't currently have logic here to avoid duplicate work if
  // multiple callers request the same tree at once.  We could store a map of
  // pending lookups as (Hash --> std::list<Promise<unique_ptr<Tree>>), and
  // just add a new Promise to the list if this Hash already exists in the
  // pending list.
  //
  // However, de-duplication of object loads will already be done at the Inode
  // layer.  Therefore we currently don't bother de-duping loads at this layer.

  // Load the tree from the BackingStore.
  return backingStore_->getTree(id).then([id](
      std::unique_ptr<Tree> loadedTree) {
    if (!loadedTree) {
      // TODO: Perhaps we should do some short-term negative caching?
      VLOG(2) << "unable to find tree " << id;
      throw std::domain_error(
          folly::to<string>("tree ", id.toString(), " not found"));
    }

    // TODO: For now, the BackingStore objects actually end up already
    // saving the Tree object in the LocalStore, so we don't do anything
    // here.
    //
    // localStore_->putTree(loadedTree.get());
    VLOG(3) << "tree " << id << " retrieved from backing store";
    return loadedTree;
  });
}

unique_ptr<Blob> ObjectStore::getBlob(const Hash& id) const {
  return getBlobFuture(id).get();
}

Future<unique_ptr<Blob>> ObjectStore::getBlobFuture(const Hash& id) const {
  auto blob = localStore_->getBlob(id);
  if (blob) {
    VLOG(4) << "blob " << id << "  found in local store";
    return makeFuture(std::move(blob));
  }

  // Look in the BackingStore
  return backingStore_->getBlob(id).then(
      [ localStore = localStore_, id ](std::unique_ptr<Blob> loadedBlob) {
        if (!loadedBlob) {
          VLOG(2) << "unable to find blob " << id;
          // TODO: Perhaps we should do some short-term negative caching?
          throw std::domain_error(
              folly::to<string>("blob ", id.toString(), " not found"));
        }

        VLOG(3) << "blob " << id << "  retrieved from backing store";
        localStore->putBlob(id, loadedBlob.get());
        return loadedBlob;
      });
}

Future<unique_ptr<Tree>> ObjectStore::getTreeForCommit(
    const Hash& commitID) const {
  VLOG(3) << "getTreeForCommit(" << commitID << ")";

  return backingStore_->getTreeForCommit(commitID).then(
      [commitID](std::unique_ptr<Tree> tree) {
        if (!tree) {
          throw std::domain_error(folly::to<string>(
              "unable to import commit ", commitID.toString()));
        }

        // For now we assume that the BackingStore will insert the Tree into the
        // LocalStore on its own, so we don't have to update the LocalStore
        // ourselves here.
        return tree;
      });
}

Hash ObjectStore::getSha1ForBlob(const Hash& id) const {
  auto metadata = getBlobMetadata(id).get();
  return metadata.sha1;
}

Future<BlobMetadata> ObjectStore::getBlobMetadata(const Hash& id) const {
  auto localData = localStore_->getBlobMetadata(id);
  if (localData.hasValue()) {
    return localData.value();
  }

  // Load the blob from the BackingStore.
  //
  // TODO: It would be nice to add a smarter API to the BackingStore so that we
  // can query it just for the blob metadata if it supports getting that
  // without retrieving the full blob data.
  return backingStore_->getBlob(id).then(
      [ localStore = localStore_, id ](std::unique_ptr<Blob> blob) {
        if (!blob) {
          // TODO: Perhaps we should do some short-term negative caching?
          throw std::domain_error(
              folly::to<string>("blob ", id.toString(), " not found"));
        }

        return localStore->putBlob(id, blob.get());
      });
}
}
} // facebook::eden
