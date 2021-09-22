/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/hg/HgDatapackStore.h"

#include <folly/Optional.h>
#include <folly/io/IOBuf.h>
#include <folly/logging/xlog.h>
#include <memory>
#include <optional>

#include "eden/fs/model/Blob.h"
#include "eden/fs/model/Hash.h"
#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeEntry.h"
#include "eden/fs/store/hg/HgImportRequest.h"
#include "eden/fs/store/hg/HgProxyHash.h"
#include "eden/fs/utils/Bug.h"

namespace facebook::eden {

namespace {
TreeEntryType fromRawTreeEntryType(RustTreeEntryType type) {
  switch (type) {
    case RustTreeEntryType::RegularFile:
      return TreeEntryType::REGULAR_FILE;
    case RustTreeEntryType::Tree:
      return TreeEntryType::TREE;
    case RustTreeEntryType::ExecutableFile:
      return TreeEntryType::EXECUTABLE_FILE;
    case RustTreeEntryType::Symlink:
      return TreeEntryType::SYMLINK;
  }
  EDEN_BUG() << "unknown tree entry type " << static_cast<uint32_t>(type)
             << " loaded from data store";
}

TreeEntry fromRawTreeEntry(
    RustTreeEntry entry,
    RelativePathPiece path,
    LocalStore::WriteBatch* writeBatch) {
  std::optional<uint64_t> size;
  std::optional<Hash> contentSha1;

  if (entry.size != nullptr) {
    size = *entry.size;
  }

  if (entry.content_sha1 != nullptr) {
    contentSha1 = Hash{*entry.content_sha1};
  }

  auto name = PathComponent(folly::StringPiece{entry.name.asByteRange()});
  auto hash = Hash{entry.hash};

  auto fullPath = path + name;
  auto proxyHash = HgProxyHash::store(fullPath, hash, writeBatch);

  return TreeEntry{
      proxyHash,
      std::move(name),
      fromRawTreeEntryType(entry.ttype),
      size,
      contentSha1};
}

FOLLY_MAYBE_UNUSED std::unique_ptr<Tree> fromRawTree(
    const RustTree* tree,
    const Hash& edenTreeId,
    RelativePathPiece path,
    LocalStore::WriteBatch* writeBatch) {
  std::vector<TreeEntry> entries;

  for (uintptr_t i = 0; i < tree->length; i++) {
    try {
      auto entry = fromRawTreeEntry(tree->entries[i], path, writeBatch);
      entries.push_back(entry);
    } catch (const PathComponentContainsDirectorySeparator& ex) {
      XLOG(WARN) << "Ignoring directory entry: " << ex.what();
    }
  }

  auto edenTree = std::make_unique<Tree>(std::move(entries), edenTreeId);
  auto serialized = LocalStore::serializeTree(*edenTree);
  writeBatch->put(KeySpace::TreeFamily, edenTreeId, serialized.coalesce());
  writeBatch->flush();

  return edenTree;
}
} // namespace

std::unique_ptr<Blob> HgDatapackStore::getBlobLocal(
    const Hash& id,
    const HgProxyHash& hgInfo) {
  auto content =
      store_.getBlob(hgInfo.path().stringPiece(), hgInfo.byteHash(), true);
  if (content) {
    return std::make_unique<Blob>(id, std::move(*content));
  }

  return nullptr;
}

std::unique_ptr<Tree> HgDatapackStore::getTreeLocal(
    const Hash& edenTreeId,
    const HgProxyHash& proxyHash,
    LocalStore& localStore) {
  auto tree = store_.getTree(proxyHash.byteHash(), /*local=*/true);
  if (tree) {
    return fromRawTree(
        tree.get(),
        edenTreeId,
        proxyHash.path(),
        localStore.beginWrite().get());
  }

  return nullptr;
}

void HgDatapackStore::getBlobBatch(
    const std::vector<std::shared_ptr<HgImportRequest>>& importRequests) {
  std::vector<std::pair<folly::ByteRange, folly::ByteRange>> requests;

  size_t count = importRequests.size();
  requests.reserve(count);

  for (const auto& importRequest : importRequests) {
    auto& proxyHash =
        importRequest->getRequest<HgImportRequest::BlobImport>()->proxyHash;
    requests.emplace_back(
        folly::ByteRange{proxyHash.path().stringPiece()}, proxyHash.byteHash());
  }

  store_.getBlobBatch(
      requests,
      false,
      [&importRequests, &requests](
          size_t index, std::unique_ptr<folly::IOBuf> content) {
        XLOGF(
            DBG9,
            "Imported name={} node={}",
            folly::StringPiece{requests[index].first},
            folly::hexlify(requests[index].second));
        auto& importRequest = importRequests[index];
        auto* blobRequest =
            importRequest->getRequest<HgImportRequest::BlobImport>();
        auto blob = std::make_unique<Blob>(blobRequest->hash, *content);
        importRequest->getPromise<std::unique_ptr<Blob>>()->setValue(
            std::move(blob));
      });
}

void HgDatapackStore::getTreeBatch(
    const std::vector<std::shared_ptr<HgImportRequest>>& importRequests,
    LocalStore::WriteBatch* writeBatch,
    std::vector<folly::Promise<std::unique_ptr<Tree>>>* promises) {
  std::vector<std::pair<folly::ByteRange, folly::ByteRange>> requests;
  requests.reserve(importRequests.size());

  for (const auto& importRequest : importRequests) {
    auto& proxyHash =
        importRequest->getRequest<HgImportRequest::TreeImport>()->proxyHash;
    requests.emplace_back(
        folly::ByteRange{proxyHash.path().stringPiece()}, proxyHash.byteHash());
  }

  store_.getTreeBatch(
      requests,
      false,
      [promises, &requests, &importRequests, writeBatch](
          size_t index, std::shared_ptr<RustTree> content) mutable {
        auto& promise = (*promises)[index];
        promise.setWith([&] {
          XLOGF(
              DBG4,
              "Imported tree name={} node={}",
              folly::StringPiece{requests[index].first},
              folly::hexlify(requests[index].second));

          auto& importRequest = importRequests[index];
          auto* treeRequest =
              importRequest->getRequest<HgImportRequest::TreeImport>();

          return fromRawTree(
              content.get(),
              treeRequest->hash,
              treeRequest->proxyHash.path(),
              writeBatch);
        });
      });
}

std::unique_ptr<Tree> HgDatapackStore::getTree(
    const RelativePath& path,
    const Hash& manifestId,
    const Hash& edenTreeId,
    LocalStore::WriteBatch* writeBatch) {
  // For root trees we will try getting the tree locally first.  This allows
  // us to catch when Mercurial might have just written a tree to the store,
  // and refresh the store so that the store can pick it up.  We don't do
  // this for all trees, as it would cause a lot of additional work on every
  // cache miss, and just doing it for root trees is sufficient to detect the
  // scenario where Mercurial just wrote a brand new tree.
  bool local_only = path.empty();
  auto tree = store_.getTree(manifestId.getBytes(), local_only);
  if (!tree && local_only) {
    // Mercurial might have just written the tree to the store. Refresh the
    // store and try again, this time allowing remote fetches.
    store_.flush();
    tree = store_.getTree(manifestId.getBytes(), false);
  }
  if (tree) {
    return fromRawTree(tree.get(), edenTreeId, path, writeBatch);
  }
  return nullptr;
}

void HgDatapackStore::flush() {
  store_.flush();
}

} // namespace facebook::eden
