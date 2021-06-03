/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/EmptyBackingStore.h"

#include <folly/futures/Future.h>
#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/store/ObjectFetchContext.h"

using folly::makeSemiFuture;
using folly::SemiFuture;
using std::unique_ptr;

namespace facebook {
namespace eden {

EmptyBackingStore::EmptyBackingStore() {}

EmptyBackingStore::~EmptyBackingStore() {}

Hash EmptyBackingStore::parseRootId(folly::StringPiece /*rootId*/) {
  throw std::domain_error("empty backing store");
}

std::string EmptyBackingStore::renderRootId(const Hash& /*rootId*/) {
  throw std::domain_error("empty backing store");
}

SemiFuture<unique_ptr<Tree>> EmptyBackingStore::getTree(
    const Hash& /* id */,
    ObjectFetchContext& /* context */) {
  return makeSemiFuture<unique_ptr<Tree>>(
      std::domain_error("empty backing store"));
}

SemiFuture<unique_ptr<Blob>> EmptyBackingStore::getBlob(
    const Hash& /* id */,
    ObjectFetchContext& /* context */) {
  return makeSemiFuture<unique_ptr<Blob>>(
      std::domain_error("empty backing store"));
}

SemiFuture<unique_ptr<Tree>> EmptyBackingStore::getTreeForCommit(
    const Hash& /* commitID */,
    ObjectFetchContext& /* context */) {
  return makeSemiFuture<unique_ptr<Tree>>(
      std::domain_error("empty backing store"));
}

} // namespace eden
} // namespace facebook
