/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once
#include <folly/File.h>
#include <folly/Optional.h>
#include <folly/Range.h>
#include <gtest/gtest_prod.h>
#include "TreeInode.h"
#include "eden/fs/utils/DirType.h"
#include "eden/fs/utils/PathFuncs.h"
#include "eden/fs/utils/PathMap.h"

namespace facebook {
namespace eden {

namespace overlay {
class OverlayDir;
}

class InodeMap;

/** Manages the write overlay storage area.
 *
 * The overlay is where we store files that are not yet part of a snapshot.
 *
 * The contents of this storage layer are overlaid on top of the object store
 * snapshot that is active in a given mount point.
 *
 * There is one overlay area associated with each eden client instance.
 *
 * We use the Overlay to manage mutating the structure of the checkout;
 * each time we create or delete a directory entry, we do so through
 * the overlay class.
 *
 * The Overlay class keeps track of the mutated tree; if we mutate some
 * file "foo/bar/baz" then the Overlay records metadata about the list
 * of files in the root, the list of files in "foo", the list of files in
 * "foo/bar" and finally materializes "foo/bar/baz".
 */
class Overlay {
 public:
  explicit Overlay(AbsolutePathPiece localDir);

  /** Returns the path to the root of the Overlay storage area */
  const AbsolutePath& getLocalDir() const;

  void saveOverlayDir(InodeNumber inodeNumber, const TreeInode::Dir& dir);

  folly::Optional<TreeInode::Dir> loadOverlayDir(
      InodeNumber inodeNumber,
      InodeMap* inodeMap);

  void removeOverlayData(InodeNumber inodeNumber);

  /**
   * Remove the overlay data for the given tree inode and recursively remove
   * everything beneath it too.
   *
   * Must only be called on trees.
   */
  void recursivelyRemoveOverlayData(InodeNumber inodeNumber);

  bool hasOverlayData(InodeNumber inodeNumber);

  /**
   * Helper function that opens an existing overlay file,
   * checks if the file has valid header
   * populates st_atim, st_mtim, st_ctim and returns the file.
   */
  folly::File openFile(
      InodeNumber inodeNumber,
      folly::StringPiece headerId,
      InodeTimestamps& timestamps);

  /**
   * Open an existing overlay file without verifying the header.
   */
  folly::File openFileNoVerify(InodeNumber inodeNumber);

  /**
   * Helper function that creates an overlay file for a new FileInode.
   */
  folly::File createOverlayFile(
      InodeNumber inodeNumber,
      const InodeTimestamps& timestamps,
      folly::ByteRange contents);

  /**
   * Helper function to write an overlay file for a FileInode with existing
   * contents.
   */
  folly::File createOverlayFile(
      InodeNumber inodeNumber,
      const InodeTimestamps& timestamps,
      const folly::IOBuf& contents);

  /**
   * Updates the timestamps of an overlay file appropriately
   * while unloading an inode.
   */
  static void updateTimestampToHeader(
      int fd,
      const InodeTimestamps& timeStamps);

  /**
   * Get the maximum inode number stored in the overlay.
   *
   * This is called when opening a mount point, to make sure that new inodes
   * handed out from this point forwards are always greater than any inodes
   * already tracked in the overlay.
   */
  InodeNumber getMaxRecordedInode();

  /**
   * Constants for an header in overlay file.
   */
  static constexpr folly::StringPiece kHeaderIdentifierDir{"OVDR"};
  static constexpr folly::StringPiece kHeaderIdentifierFile{"OVFL"};
  static constexpr uint32_t kHeaderVersion = 1;
  static constexpr size_t kHeaderLength = 64;

 private:
  /**
   * The maximum path length for the path to a file inside the overlay
   * directory.
   *
   * This is 2 bytes for the initial subdirectory name, 1 byte for the '/',
   * 20 bytes for the inode number, and 1 byte for a null terminator.
   */
  static constexpr size_t kMaxPathLength = 24;

  FRIEND_TEST(OverlayTest, getFilePath);
  using InodePath = std::array<char, kMaxPathLength>;

  void initOverlay();
  bool isOldFormatOverlay() const;
  void readExistingOverlay(int infoFD);
  void initNewOverlay();
  folly::Optional<overlay::OverlayDir> deserializeOverlayDir(
      InodeNumber inodeNumber,
      InodeTimestamps& timeStamps) const;

  /**
   * Creates header for the files stored in Overlay
   */
  static std::array<uint8_t, kHeaderLength> createHeader(
      folly::StringPiece identifier,
      uint32_t version,
      const InodeTimestamps& timestamps);

  folly::File
  createOverlayFileImpl(InodeNumber inodeNumber, iovec* iov, size_t iovCount);

  /**
   * Get the path to the file for the given inode, relative to localDir_.
   *
   * This puts the path name data into the user-supplied InodePath object.  A
   * null terminator will be added to the path.
   *
   * Returns the length of the path name, not including the terminating null
   * byte.
   */
  static size_t getFilePath(InodeNumber inodeNumber, InodePath& outPath);

  /**
   * Parses, validates and reads Timestamps from the header.
   */
  static void parseHeader(
      folly::StringPiece header,
      folly::StringPiece headerId,
      InodeTimestamps& timeStamps);

  /** path to ".eden/CLIENT/local" */
  AbsolutePath localDir_;

  /**
   * An open file descriptor to the overlay info file.
   *
   * This is primarily used to hold a lock on the overlay for as long as we are
   * using it.  We want to ensure that only one eden process
   */
  folly::File infoFile_;

  /**
   * An open file to the overlay directory.
   *
   * We maintain this so we can use openat(), unlinkat(), etc.
   */
  folly::File dirFile_;
};
} // namespace eden
} // namespace facebook
