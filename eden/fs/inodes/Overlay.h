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
#include <folly/Range.h>
#include <folly/futures/Future.h>
#include <folly/futures/Promise.h>
#include <array>
#include <condition_variable>
#include <optional>
#include <thread>
#include "eden/fs/fuse/InodeNumber.h"
#include "eden/fs/inodes/overlay/FsOverlay.h"
#include "eden/fs/inodes/overlay/gen-cpp2/overlay_types.h"
#include "eden/fs/utils/DirType.h"
#include "eden/fs/utils/PathFuncs.h"

namespace facebook {
namespace eden {

namespace overlay {
class OverlayDir;
}

struct DirContents;
class InodeMap;
struct InodeMetadata;
template <typename T>
class InodeTable;
using InodeMetadataTable = InodeTable<InodeMetadata>;
struct SerializedInodeMap;

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
  /**
   * Create a new Overlay object.
   *
   * The caller must call initialize() after creating the Overlay and wait for
   * it to succeed before using any other methods.
   */
  explicit Overlay(AbsolutePathPiece localDir);
  ~Overlay();

  Overlay(const Overlay&) = delete;
  Overlay(Overlay&&) = delete;
  Overlay& operator=(const Overlay&) = delete;
  Overlay& operator=(Overlay&&) = delete;

  /**
   * Initialize the overlay.
   *
   * This must be called after the Overlay constructor, before performing
   * operations on the overlay.
   *
   * This may be a slow operation and may perform significant amounts of
   * disk I/O.
   *
   * The initialization operation may include:
   * - Acquiring a lock to ensure no other processes are accessing the on-disk
   *   overlay state
   * - Creating the initial on-disk overlay data structures if necessary.
   * - Verifying and fixing the on-disk data if the Overlay was not shut down
   *   cleanly the last time it was opened.
   * - Upgrading the on-disk data from older formats if the Overlay was created
   *   by an older version of the software.
   */
  folly::SemiFuture<folly::Unit> initialize();

  /**
   * Closes the overlay. It is undefined behavior to access the
   * InodeMetadataTable concurrently or call any other Overlay method
   * concurrently with or after calling close(). The Overlay will try to detect
   * this with assertions but cannot always detect concurrent access.
   *
   * Returns the next available InodeNumber to be passed to any process taking
   * over an Eden mount.
   */
  void close();

  /**
   * Get the maximum inode number that has ever been allocated to an inode.
   */
  InodeNumber getMaxInodeNumber();

  /**
   * allocateInodeNumber() should only be called by TreeInode.
   *
   * This can be called:
   * - To allocate an inode number for an existing tree entry that does not
   *   need to be loaded yet.
   * - To allocate an inode number for a brand new inode being created by
   *   TreeInode::create() or TreeInode::mkdir().  In this case
   *   inodeCreated() should be called immediately afterwards to register the
   *   new child Inode object.
   *
   * TODO: It would be easy to extend this function to allocate a range of
   * inode values in one atomic operation.
   */
  InodeNumber allocateInodeNumber();

  /**
   * Returns an InodeMetadataTable for accessing and storing inode metadata.
   * Owned by the Overlay so records can be removed when the Overlay discovers
   * it no longer needs data for an inode or its children.
   */
  InodeMetadataTable* getInodeMetadataTable() const {
    return inodeMetadataTable_.get();
  }

  void saveOverlayDir(InodeNumber inodeNumber, const DirContents& dir);

  std::optional<DirContents> loadOverlayDir(InodeNumber inodeNumber);

  void removeOverlayData(InodeNumber inodeNumber);

  /**
   * Remove the overlay data for the given tree inode and recursively remove
   * everything beneath it too.
   *
   * Must only be called on trees.
   */
  void recursivelyRemoveOverlayData(InodeNumber inodeNumber);

  /**
   * Returns a future that completes once all previously-issued async
   * operations, namely recursivelyRemoveOverlayData, finish.
   */
  folly::Future<folly::Unit> flushPendingAsync();

  bool hasOverlayData(InodeNumber inodeNumber);

  /**
   * Helper function that opens an existing overlay file,
   * checks if the file has valid header, and returns the file.
   */
  folly::File openFile(InodeNumber inodeNumber, folly::StringPiece headerId);

  /**
   * Open an existing overlay file without verifying the header.
   */
  folly::File openFileNoVerify(InodeNumber inodeNumber);

  /**
   * Helper function that creates an overlay file for a new FileInode.
   */
  folly::File createOverlayFile(
      InodeNumber inodeNumber,
      folly::ByteRange contents);

  /**
   * Helper function to write an overlay file for a FileInode with existing
   * contents.
   */
  folly::File createOverlayFile(
      InodeNumber inodeNumber,
      const folly::IOBuf& contents);

 private:
  /**
   * A request for the background GC thread.  There are two types of requests:
   * recursively forget data underneath an given directory, or complete a
   * promise.  The latter is used for synchronization with the GC thread,
   * primarily in unit tests.
   *
   * If additional request types are added in the future, consider renaming to
   * AsyncRequest.  However, recursive collection of forgotten inode numbers
   * is the only operation that can be made async while preserving our
   * durability goals.
   */
  struct GCRequest {
    GCRequest() {}
    explicit GCRequest(overlay::OverlayDir&& d) : dir{std::move(d)} {}
    explicit GCRequest(folly::Promise<folly::Unit> p) : flush{std::move(p)} {}

    overlay::OverlayDir dir;
    // Iff set, this is a flush request.
    std::optional<folly::Promise<folly::Unit>> flush;
  };

  struct GCQueue {
    bool stop = false;
    std::vector<GCRequest> queue;
  };

  void initOverlay();
  void gcThread() noexcept;
  void handleGCRequest(GCRequest& request);

  /**
   * The next inode number to allocate.  Zero indicates that neither
   * initializeFromTakeover nor getMaxRecordedInode have been called.
   *
   * This value will never be 1.
   */
  std::atomic<uint64_t> nextInodeNumber_{0};

  FsOverlay fsOverlay_;

  /**
   * Disk-backed mapping from inode number to InodeMetadata.
   * Defined below fsOverlay_ because it acquires its own file lock, which
   * should be released first during shutdown.
   */
  std::unique_ptr<InodeMetadataTable> inodeMetadataTable_;

  /**
   * Thread which recursively removes entries from the overlay underneath the
   * trees added to gcQueue_.
   */
  std::thread gcThread_;
  folly::Synchronized<GCQueue, std::mutex> gcQueue_;
  std::condition_variable gcCondVar_;
};

} // namespace eden
} // namespace facebook
