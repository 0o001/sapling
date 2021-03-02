/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once
#include <folly/Synchronized.h>

#include "eden/fs/inodes/IOverlay.h"
#include "eden/fs/inodes/overlay/gen-cpp2/overlay_types.h"
#include "eden/fs/sqlite/Sqlite.h"

namespace facebook {
namespace eden {
struct InodeNumber;
using LockedDbPtr = folly::Synchronized<sqlite3*>::LockedPtr;

/**
 * Sqlite overlay stores the directory inode and its entries in the sqlite
 * database. This is similar to FsOverlay but doesn't support all the
 * functionality. This is only used on Windows right now.
 */

class SqliteOverlay : public IOverlay {
 public:
  explicit SqliteOverlay(AbsolutePathPiece localDir);

  ~SqliteOverlay();

  SqliteOverlay(const SqliteOverlay&) = delete;
  SqliteOverlay& operator=(const SqliteOverlay&) = delete;
  SqliteOverlay(SqliteOverlay&&) = delete;
  SqliteOverlay&& operator=(SqliteOverlay&&) = delete;

  bool supportsSemanticOperations() const override {
    return false;
  }

  /**
   * Initialize the overlay, and load the nextInodeNumber. The "close"
   * method should be used to release these resources and persist the
   * nextInodeNumber.
   *
   * It ignores the value of createIfNonExisting. The Sqlite
   * DB and the tables are created or opened in the contructor and are closed in
   * the destructor.
   */
  std::optional<InodeNumber> initOverlay(bool createIfNonExisting) override;

  /**
   *  Gracefully, shutdown the overlay, persisting the overlay's
   * nextInodeNumber.
   */
  void close(std::optional<InodeNumber> nextInodeNumber) override;

  const AbsolutePath& getLocalDir() const override {
    return localDir_;
  }

  /**
   * Was FsOverlay initialized - i.e., is cleanup (close) necessary.
   */
  bool initialized() const override {
    return bool(db_);
  }

  void saveOverlayDir(InodeNumber inodeNumber, const overlay::OverlayDir& odir)
      override;
  std::optional<overlay::OverlayDir> loadOverlayDir(
      InodeNumber inodeNumber) override;

  void removeOverlayData(InodeNumber inodeNumber) override;
  bool hasOverlayData(InodeNumber inodeNumber) override;

  /**
   * Update the last used Inode number to a new value. This is a stop gap
   * solution for the recovery when Eden doesn't know the last used inode number
   * in case of an unclean shutdown.
   *
   * How it works: The SqliteOverlay allocates a range of inodes and keep
   * assigning the inode numbers from that. Once the allocated inode number is
   * at the end of range it will allocate a new range. To allocate the range it
   * will add the the known value of the last used inode number with the size of
   * range and save that value as the last known Inode number. In case of
   * unclean shutdown we know that last used inode number must be smaller than
   * the Inode number stored in the Sqlite.
   *
   */
  void updateUsedInodeNumber(uint64_t usedInodeNumber) override;

 private:
  std::optional<std::string> load(uint64_t inodeNumber) const;
  bool hasInode(uint64_t inodeNumber) const;
  void save(uint64_t inodeNumber, bool isDirectory, folly::ByteRange value);

  // APIs to fetch and save the value of next Inode number
  void saveNextInodeNumber(uint64_t inodeNumber);
  std::optional<uint64_t> readNextInodeNumber(LockedDbPtr& db);
  void writeNextInodeNumber(LockedDbPtr& db, uint64_t inodeNumber);

  // Sqlite db handle
  std::unique_ptr<SqliteDatabase> db_;

  // Path to the folder containing DB.
  const AbsolutePath localDir_;

  // nextInodeNumber_ is part of a stop gap solution for Windows described
  // above. The writes to this are protected by db_ lock.
  std::atomic<uint64_t> nextInodeNumber_{0};
}; // namespace eden

} // namespace eden
} // namespace facebook
