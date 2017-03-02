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
#include <folly/io/IOBuf.h>
#include <mutex>
#include "eden/fs/inodes/TreeInode.h"
#include "eden/fs/model/Tree.h"

namespace facebook {
namespace eden {
namespace fusell {
class BufVec;
}

class Blob;
class FileInode;
class Hash;
class Overlay;

/**
 * FileData stores information about a file contents.
 *
 * The data may be lazily loaded from the EdenMount's ObjectStore only when it
 * is needed.
 *
 * FileData objects are tracked via shared_ptr.  FileInode and FileHandle
 * objects maintain references to them.  FileData objects never outlive
 * the FileInode to which they belong.
 */
class FileData {
 public:
  /** Construct a FileData from an overlay entry */
  FileData(FileInode* inode, std::mutex& mutex, TreeInode::Entry* entry);

  /** Construct a freshly created FileData from a pre-opened File object.
   * file must be moved in (it has no copy constructor) and must have
   * been created by a call to Overlay::createFile.  This constructor
   * is used in the TreeInode::create case and is required to implement
   * O_EXCL correctly. */
  FileData(
      FileInode* inode,
      std::mutex& mutex,
      TreeInode::Entry* entry,
      folly::File&& file);

  /**
   * Read up to size bytes from the file at the specified offset.
   *
   * Returns an IOBuf containing the data.  This may return fewer bytes than
   * requested.  If the specified offset is at or past the end of the buffer an
   * empty IOBuf will be returned.  Otherwise between 1 and size bytes will be
   * returned.  If fewer than size bytes are returned this does *not* guarantee
   * that the end of the file was reached.
   *
   * May throw exceptions on error.
   */
  std::unique_ptr<folly::IOBuf> readIntoBuffer(size_t size, off_t off);
  fusell::BufVec read(size_t size, off_t off);
  size_t write(fusell::BufVec&& buf, off_t off);
  size_t write(folly::StringPiece data, off_t off);
  struct stat stat();
  void flush(uint64_t lock_owner);
  void fsync(bool datasync);

  /// Change attributes for this inode.
  // attr is a standard struct stat.  Only the members indicated
  // by to_set are valid.  Defined values for the to_set flags
  // are found in the fuse_lowlevel.h header file and have symbolic
  // names matching FUSE_SET_*.
  struct stat setAttr(const struct stat& attr, int to_set);

  /// Returns the sha1 hash of the content.
  Hash getSha1();
  /// Returns the sha1 hash of the content, for existing lock holders.
  Hash getSha1Locked(const std::unique_lock<std::mutex>&);

  /**
   * Read the entire file contents, and return them as a string.
   *
   * Note that this API generally should only be used for fairly small files.
   */
  std::string readAll();

  /**
   * Materialize the file data.
   * openFlags has the same meaning as the flags parameter to
   * open(2).  Materialization depends on the write mode specified
   * in those flags; if we are writing to the file then we need to
   * copy it locally to the overlay.  If we are truncating we just
   * need to create an empty file in the overlay.  Otherwise we
   * need to go out to the LocalStore to obtain the backing data.
   *
   * TODO: The overlay argument should be passed in as a raw pointer.  We do
   * not need ownership of it.
   */
  void materializeForWrite(int openFlags);

  /**
   * Materializes the file data.
   *
   * This variant is optimized for the read case; if there is
   * no locally available version of the file in the overlay,
   * this method will fetch it from the LocalStore.
   *
   * TODO: The overlay argument should be passed in as a raw pointer.  We do
   * not need ownership of it.
   */
  void materializeForRead(int openFlags);

 private:
  ObjectStore* getObjectStore() const;

  /// Recompute the SHA1 content hash of the open file_.
  // The mutex must be owned by the caller.
  Hash recomputeAndStoreSha1();

  /**
   * The FileInode that this FileData object belongs to.
   *
   * This pointer never changes once a FileData object is constructed.  A
   * FileData always belongs to the same FileInode.  Therefore it is safe to
   * access this pointer without locking.
   */
  FileInode* const inode_{nullptr};

  /**
   * Reference to the mutex in the associated inode.
   * It must be held by readers and writers before interpreting the filedata,
   * as any actor may cause materialization or truncation of the data.
   * Recommended practice in the implementation of methods on this class is to
   * hold a unique_lock as a guard for the duration of the method.
   *
   * TODO: Maybe we should just make FileData a friend of FileInode,
   * and access this as inode_->mutex_
   */
  std::mutex& mutex_;

  /** Metadata about the file.
   * This points to the entry that is owned by the parent TreeInode
   * of this file.  It will always be non-null.
   *
   * TODO: Maybe we should just make FileData a friend of FileInode,
   * and access this as inode_->entry_
   */
  TreeInode::Entry* entry_{nullptr};

  /// if backed by tree, the data from the tree, else nullptr.
  std::unique_ptr<Blob> blob_;

  /// if backed by an overlay file, the open file descriptor
  folly::File file_;

  /// if backed by an overlay file, whether the sha1 xattr is valid
  bool sha1Valid_{false};
};
}
}
