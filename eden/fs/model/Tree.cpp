/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "Tree.h"
#include <folly/io/IOBuf.h>

namespace facebook::eden {
using namespace folly;
using namespace folly::io;

bool operator==(const Tree& tree1, const Tree& tree2) {
  return (tree1.getHash() == tree2.getHash()) &&
      (tree1.getTreeEntries() == tree2.getTreeEntries());
}

bool operator!=(const Tree& tree1, const Tree& tree2) {
  return !(tree1 == tree2);
}

size_t Tree::getSizeBytes() const {
  // TODO: we should consider using a standard memory framework across
  // eden for this type of thing. D17174143 is one such idea.
  size_t internal_size = sizeof(*this);

  size_t indirect_size =
      folly::goodMallocSize(sizeof(TreeEntry) * entries_.capacity());

  for (auto& entry : entries_) {
    indirect_size += entry.getIndirectSizeBytes();
  }
  return internal_size + indirect_size;
}

IOBuf Tree::serialize() const {
  size_t serialized_size = sizeof(uint32_t) + sizeof(uint32_t);
  for (auto& entry : entries_) {
    serialized_size += entry.serializedSize();
  }
  IOBuf buf(IOBuf::CREATE, serialized_size);
  Appender appender(&buf, 0);

  XCHECK_LE(entries_.size(), std::numeric_limits<uint32_t>::max());
  uint32_t numberOfEntries = static_cast<uint32_t>(entries_.size());

  appender.write<uint32_t>(V1_VERSION);
  appender.write<uint32_t>(numberOfEntries);
  for (auto& entry : entries_) {
    entry.serialize(appender);
  }
  return buf;
}

std::optional<Tree> Tree::tryDeserialize(
    ObjectId hash,
    folly::StringPiece data) {
  if (data.size() < sizeof(uint32_t)) {
    XLOG(ERR) << "Can not read tree version, bytes remaining " << data.size();
    return std::nullopt;
  }
  uint32_t version;
  memcpy(&version, data.data(), sizeof(uint32_t));
  data.advance(sizeof(uint32_t));
  if (version != V1_VERSION) {
    return std::nullopt;
  }

  if (data.size() < sizeof(uint32_t)) {
    XLOG(ERR) << "Can not read tree size, bytes remaining " << data.size();
    return std::nullopt;
  }
  uint32_t num_entries;
  memcpy(&num_entries, data.data(), sizeof(uint32_t));
  data.advance(sizeof(uint32_t));

  std::vector<TreeEntry> entries;
  entries.reserve(num_entries);
  for (size_t i = 0; i < num_entries; i++) {
    auto entry = TreeEntry::deserialize(data);
    if (!entry) {
      return std::nullopt;
    }
    entries.push_back(*entry);
  }

  if (data.size() != 0u) {
    XLOG(ERR) << "Corrupted tree data, extra bytes remaining " << data.size();
    return std::nullopt;
  }

  return Tree(std::move(entries), std::move(hash));
}

} // namespace facebook::eden
