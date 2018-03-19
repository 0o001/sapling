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
#include <folly/Synchronized.h>
#include <folly/futures/SharedPromise.h>
#include <chrono>
#include "eden/fs/inodes/InodeBase.h"
#include "eden/fs/model/Tree.h"

namespace folly {
class File;
}

namespace facebook {
namespace eden {

namespace fusell {
class BufVec;
}

class Blob;
class EdenFileHandle;
class Hash;
class ObjectStore;

class FileInode : public InodeBase {
 public:
  using FileHandlePtr = std::shared_ptr<EdenFileHandle>;

  enum : int { WRONG_TYPE_ERRNO = EISDIR };

  /**
   * The FUSE create request wants both the inode and a file handle.  This
   * constructor simultaneously allocates a FileInode given the File and
   * returns a new EdenFileHandle to it.
   */
  static std::tuple<FileInodePtr, FileHandlePtr> create(
      fusell::InodeNumber ino,
      TreeInodePtr parentInode,
      PathComponentPiece name,
      mode_t mode,
      folly::File&& file,
      timespec ctime);

  /**
   * If hash is none, this opens the file in the overlay and leaves the inode
   * in MATERIALIZED_IN_OVERLAY state.  If hash is set, the inode is in
   * NOT_LOADED state.
   */
  FileInode(
      fusell::InodeNumber ino,
      TreeInodePtr parentInode,
      PathComponentPiece name,
      mode_t mode,
      const folly::Optional<Hash>& hash);

  /**
   * Construct an inode using a freshly created overlay file.
   * file must be moved in and must have been created by a call to
   * Overlay::openFile.
   */
  FileInode(
      fusell::InodeNumber ino,
      TreeInodePtr parentInode,
      PathComponentPiece name,
      mode_t mode,
      folly::File&& file,
      timespec ctime);

  folly::Future<fusell::Dispatcher::Attr> getattr() override;

  /// Throws InodeError EINVAL if inode is not a symbolic node.
  folly::Future<std::string> readlink();

  folly::Future<std::shared_ptr<fusell::FileHandle>> open(int flags);

  folly::Future<std::vector<std::string>> listxattr() override;
  folly::Future<std::string> getxattr(folly::StringPiece name) override;

  folly::Future<folly::Unit> prefetch() override;

  /**
   * Updates in-memory timestamps in FileInode and TreeInode to the overlay
   * file.
   */
  void updateOverlayHeader() const override;
  folly::Future<Hash> getSha1();

  /**
   * Compute the path to the overlay file for this item.
   */
  AbsolutePath getLocalPath() const;

  /**
   * Check to see if the file has the same contents as the specified blob
   * and the same tree entry type.
   *
   * This is more efficient than manually comparing the contents, as it can
   * perform a simple hash check if the file is not materialized.
   */
  bool isSameAs(const Blob& blob, TreeEntryType entryType);
  folly::Future<bool> isSameAs(const Hash& blobID, TreeEntryType entryType);

  /**
   * Get the file mode_t value.
   */
  mode_t getMode() const;

  /**
   * Get the file dev_t value.
   */
  dev_t getRdev() const;

  /**
   * Get the permissions bits from the file mode.
   *
   * This returns the mode with the file type bits masked out.
   */
  mode_t getPermissions() const;

  /**
   * If this file is backed by a source control Blob, return the hash of the
   * Blob, or return folly::none if this file is materialized in the overlay.
   *
   * Beware that the file's materialization state may have changed by the time
   * you use the return value of this method.  This method is primarily
   * intended for use in tests and debugging functions.  Its return value
   * generally cannot be trusted in situations where there may be concurrent
   * modifications by other threads.
   */
  folly::Optional<Hash> getBlobHash() const;

  /**
   * Read the entire file contents, and return them as a string.
   *
   * Note that this API generally should only be used for fairly small files.
   */
  FOLLY_NODISCARD folly::Future<std::string> readAll();

  folly::Future<size_t> write(folly::StringPiece data, off_t off);

  /**
   * Get the timestamps of the inode.
   */
  InodeTimestamps getTimestamps() const;

 private:
  /**
   * Load the file data so it can be used for reading.
   *
   * If this file is materialized, this opens its file in the overlay.
   * If the file is not materialized, this loads the Blob data from the
   * ObjectStore.
   *
   * Returns a FileHandle that, while it's alive, either State::blob is non-null
   * or getFile() will return a File handle.
   */
  FOLLY_NODISCARD folly::Future<FileHandlePtr> ensureDataLoaded();

  /**
   * Materialize the file data.  If already materialized, the future is
   * immediately fulfilled.  Otherwise, the backing blob is loaded and copied
   * into the overlay.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> materializeForWrite();

  /**
   * Ensures the inode transitions to or stays in the 'materialized' state,
   * and truncates the file to zero bytes.
   */
  void materializeAndTruncate();

  /**
   * The contents of a FileInode.
   *
   * This structure exists to allow the entire contents to be protected inside
   * folly::Synchronized.  This ensures proper synchronization when accessing
   * any member variables of FileInode.
   *
   * A FileInode can be in one of three states:
   *   - not loaded
   *   - loading: fetching data from backing store, but it's not available yet
   *   - loaded: contents has been imported from mercurial and is accessible
   *   - materialized: contents are written into overlay and file handle is open
   *
   * Valid state transitions:
   *   - not loaded -> loading
   *   - not loaded -> materialized (O_TRUNC)
   *   - loading -> loaded
   *   - loading -> materialized (O_TRUNC)
   *   - loaded -> materialized
   */
  struct State {
    enum Tag : uint8_t {
      NOT_LOADED,
      BLOB_LOADING,
      BLOB_LOADED,
      MATERIALIZED_IN_OVERLAY,
    };

    State(
        FileInode* inode,
        mode_t mode,
        const folly::Optional<Hash>& hash,
        const timespec& lastCheckoutTime);
    State(FileInode* inode, mode_t mode, const timespec& creationTime);
    ~State();

    /**
     * In lieu of std::variant, enforce the state machine invariants.
     * Call after construction and on every modification.
     */
    void checkInvariants();

    /**
     * Returns true if the file is materialized in the overlay.
     */
    bool isMaterialized() const {
      return tag == MATERIALIZED_IN_OVERLAY;
    }

    /**
     * Returns true if we're maintaining an open file.
     */
    bool isFileOpen() const {
      return bool(file);
    }

    /**
     * Close out the internal file descriptor.
     */
    void closeFile();

    Tag tag;

    mode_t mode;

    /**
     * Set only in 'not loaded', 'loading', and 'loaded' states, none otherwise.
     * TODO: Perhaps we ought to simply leave this defined...
     */
    folly::Optional<Hash> hash;

    /**
     * Set if 'loading'.
     */
    folly::Optional<folly::SharedPromise<FileHandlePtr>> blobLoadingPromise;

    /**
     * Set if 'loaded', references immutable data from the backing store.
     */
    std::shared_ptr<const Blob> blob;

    /**
     * If backed by an overlay file, whether the sha1 xattr is valid
     */
    bool sha1Valid{false};

    /**
     * Set if 'materialized', holds the open file descriptor backed by an
     * overlay file.
     */
    folly::File file;

    /**
     * Number of open file handles referencing us.
     */
    size_t openCount{0};

    /**
     * Timestamps for FileInode.
     */
    InodeTimestamps timeStamps;
  };

  /**
   * Get a FileInodePtr to ourself.
   *
   * This uses FileInodePtr::newPtrFromExisting() internally.
   *
   * This should only be called in contexts where we know an external caller
   * already has an existing reference to us.  (Which is most places--a caller
   * has to have a reference to us in order to call any of our APIs.)
   */
  FileInodePtr inodePtrFromThis() {
    return FileInodePtr::newPtrFromExisting(this);
  }

  /**
   * Mark this FileInode materialized in its parent directory.
   */
  void materializeInParent();

  /**
   * Called as part of setting up an open file handle.
   */
  static void fileHandleDidOpen(State& state);

  /**
   * Called as part of shutting down an open handle.
   */
  void fileHandleDidClose();

  /**
   * Returns a file handle on the materialized file.
   * The file handle may be a transient handle or may be our own
   * local file instance, depending on whether we consider the
   * file to be open or not.  Since the caller can not easily
   * tell which is the case, the file should only be accessed
   * while the caller holds the lock on the state.
   */
  folly::File getFile(FileInode::State& state) const;

  /**
   * Helper function for isSameAs().
   *
   * This does the initial portion of the check which never requires a Future.
   * Returns Optional<bool> if the check completes immediately, or
   * folly::none if the contents need to be checked against sha1 of file
   * contents.
   */
  folly::Optional<bool> isSameAsFast(
      const Hash& blobID,
      TreeEntryType entryType);

  /**
   * Recompute the SHA1 content hash of the open file.
   */
  Hash recomputeAndStoreSha1(
      const folly::Synchronized<FileInode::State>::LockedPtr& state,
      const folly::File& file);

  ObjectStore* getObjectStore() const;
  static void storeSha1(
      const folly::Synchronized<FileInode::State>::LockedPtr& state,
      const folly::File& file,
      Hash sha1);

  /**
   * Read up to size bytes from the file at the specified offset.
   *
   * Returns a BufVec containing the data.  This may return fewer bytes than
   * requested.  If the specified offset is at or past the end of the buffer an
   * empty IOBuf will be returned.  Otherwise between 1 and size bytes will be
   * returned.  If fewer than size bytes are returned this does *not* guarantee
   * that the end of the file was reached.
   *
   * May throw exceptions on error.
   *
   * Precondition: openCount > 0.  This is held because read is only called by
   * FileInode or FileHandle.
   */
  fusell::BufVec read(size_t size, off_t off);

  folly::Future<size_t> write(fusell::BufVec&& buf, off_t off);

  folly::Future<struct stat> stat();
  void flush(uint64_t lock_owner);
  void fsync(bool datasync);

  /**
   * Update the st_blocks field in a stat structure based on the st_size value.
   */
  static void updateBlockCount(struct stat& st);

  /**
   * Helper function used in setattr to perform FileInode specific operations
   * during setattr.
   */
  folly::Future<fusell::Dispatcher::Attr> setInodeAttr(
      const fuse_setattr_in& attr) override;

  folly::Synchronized<State> state_;

  friend class ::facebook::eden::EdenFileHandle;
};
} // namespace eden
} // namespace facebook
