/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once
#include <errno.h>
#include <folly/Exception.h>
#include <folly/Portability.h>
#include <folly/Range.h>
#include <folly/futures/Future.h>
#include "eden/fuse/fuse_headers.h"
#include "eden/utils/PathFuncs.h"

namespace facebook {
namespace eden {
namespace fusell {

#define FUSELL_NOT_IMPL()                                               \
  do {                                                                  \
    LOG_FIRST_N(ERROR, 1) << __PRETTY_FUNCTION__ << " not implemented"; \
    folly::throwSystemErrorExplicit(ENOSYS, __PRETTY_FUNCTION__);       \
  } while (0)


class Dispatcher;
class SessionDeleter;
class Channel;
class RequestData;
class FileHandle;
class DirHandle;

class Dispatcher {
  fuse_conn_info connInfo_;
  Channel* chan_{nullptr};

 public:
  virtual ~Dispatcher();

  static void disp_init(void* userdata, struct fuse_conn_info* conn);
  std::unique_ptr<fuse_session, SessionDeleter> makeSession(
      Channel& channel,
      bool debug);
  Channel& getChannel() const;
  const fuse_conn_info& getConnInfo() const;

  /**
   * Called during filesystem mounting.  It informs the filesystem
   * of kernel capabilities and provides an opportunity to poke some
   * flags and limits in the conn_info to report capabilities back
   * to the kernel
   */
  virtual void initConnection(fuse_conn_info& conn);

  /**
   * Called when fuse is tearing down the session
   */
  virtual void destroy();

  /**
   * Lookup a directory entry by name and get its attributes
   */
  virtual folly::Future<fuse_entry_param> lookup(
      fuse_ino_t parent,
      PathComponentPiece name);

  /**
   * Forget about an inode
   *
   * The nlookup parameter indicates the number of lookups
   * previously performed on this inode.
   *
   * If the filesystem implements inode lifetimes, it is recommended
   * that inodes acquire a single reference on each lookup, and lose
   * nlookup references on each forget.
   *
   * The filesystem may ignore forget calls, if the inodes don't
   * need to have a limited lifetime.
   *
   * On unmount it is not guaranteed, that all referenced inodes
   * will receive a forget message.
   *
   * @param ino the inode number
   * @param nlookup the number of lookups to forget
   */
  virtual folly::Future<folly::Unit> forget(fuse_ino_t ino,
                                            unsigned long nlookup);

  /**
   * The stat information and the cache TTL for the kernel
   *
   * The timeout value is measured in seconds and indicates how long
   * the kernel side of the FUSE will cache the values in the
   * struct stat before calling getattr() again to refresh it.
   */
  struct Attr {
    struct stat st;
    double timeout;

    Attr();
  };

  /**
   * Get file attributes
   *
   * @param ino the inode number
   */
  virtual folly::Future<Attr> getattr(fuse_ino_t ino);

  /**
   * Set file attributes
   *
   * In the 'attr' argument only members indicated by the 'to_set'
   * bitmask contain valid values.  Other members contain undefined
   * values.
   *
   * @param ino the inode number
   * @param attr the attributes
   * @param to_set bit mask of attributes which should be set
   *
   * Changed in version 2.5:
   *     file information filled in for ftruncate
   */
  virtual folly::Future<Attr> setattr(fuse_ino_t ino,
                                      const struct stat& attr,
                                      int to_set);

  /**
   * Read symbolic link
   *
   * @param ino the inode number
   */
  virtual folly::Future<std::string> readlink(fuse_ino_t ino);

  /**
   * Create file node
   *
   * Create a regular file, character device, block device, fifo or
   * socket node.
   *
   * @param parent inode number of the parent directory
   * @param name to create
   * @param mode file type and mode with which to create the new file
   * @param rdev the device number (only valid if created file is a device)
   */
  virtual folly::Future<fuse_entry_param>
  mknod(fuse_ino_t parent, PathComponentPiece name, mode_t mode, dev_t rdev);

  /**
   * Create a directory
   *
   * @param parent inode number of the parent directory
   * @param name to create
   * @param mode with which to create the new file
   */
  virtual folly::Future<fuse_entry_param>
  mkdir(fuse_ino_t parent, PathComponentPiece name, mode_t mode);

  /**
   * Remove a file
   *
   * @param parent inode number of the parent directory
   * @param name to remove
   */
  virtual folly::Future<folly::Unit> unlink(
      fuse_ino_t parent,
      PathComponentPiece name);

  /**
   * Remove a directory
   *
   * @param parent inode number of the parent directory
   * @param name to remove
   */
  virtual folly::Future<folly::Unit> rmdir(
      fuse_ino_t parent,
      PathComponentPiece name);

  /**
   * Create a symbolic link
   *
   * @param link the contents of the symbolic link
   * @param parent inode number of the parent directory
   * @param name to create
   */
  virtual folly::Future<fuse_entry_param>
  symlink(PathComponentPiece link, fuse_ino_t parent, PathComponentPiece name);

  /**
   * Rename a file
   *
   * @param parent inode number of the old parent directory
   * @param name old name
   * @param newparent inode number of the new parent directory
   * @param newname new name
   */
  virtual folly::Future<folly::Unit> rename(
      fuse_ino_t parent,
      PathComponentPiece name,
      fuse_ino_t newparent,
      PathComponentPiece newname);

  /**
   * Create a hard link
   *
   * @param ino the old inode number
   * @param newparent inode number of the new parent directory
   * @param newname new name to create
   */
  virtual folly::Future<fuse_entry_param>
  link(fuse_ino_t ino, fuse_ino_t newparent, PathComponentPiece newname);

  /**
   * Open a file
   *
   * Open flags (with the exception of O_CREAT, O_EXCL, O_NOCTTY and
   * O_TRUNC) are available in fi->flags.
   *
   * Filesystem may store an arbitrary file handle (pointer, index,
   * etc) in fi->fh, and use this in other all other file operations
   * (read, write, flush, release, fsync).
   *
   * There are also some flags (direct_io, keep_cache) which the
   * filesystem may set in fi, to change the way the file is opened.
   * See fuse_file_info structure in <fuse_common.h> for more details.
   *
   * @param ino the inode number
   * @param fi file information
   */
  virtual folly::Future<FileHandle*> open(fuse_ino_t ino,
                                          const struct fuse_file_info& fi);

  /**
   * Open a directory
   *
   * Filesystem may store an arbitrary file handle (pointer, index,
   * etc) in fi->fh, and use this in other all other directory
   * stream operations (readdir, releasedir, fsyncdir).
   *
   * Filesystem may also implement stateless directory I/O and not
   * store anything in fi->fh, though that makes it impossible to
   * implement standard conforming directory stream operations in
   * case the contents of the directory can change between opendir
   * and releasedir.
   *
   * @param ino the inode number
   * @param fi file information
   */
  virtual folly::Future<DirHandle*> opendir(fuse_ino_t ino,
                                            const struct fuse_file_info& fi);

  /**
   * Get file system statistics
   *
   * @param ino the inode number, zero means "undefined"
   */
  virtual folly::Future<struct statvfs> statfs(fuse_ino_t ino);

  /**
   * Set an extended attribute
   */
  virtual folly::Future<folly::Unit> setxattr(fuse_ino_t ino,
                                              folly::StringPiece name,
                                              folly::StringPiece value,
                                              int flags);
  /**
   * Get an extended attribute
   */
  virtual folly::Future<std::string> getxattr(fuse_ino_t ino, folly::StringPiece name);
  static const int kENOATTR;

  /**
   * List extended attribute names
   */
  virtual folly::Future<std::vector<std::string>> listxattr(fuse_ino_t ino);

  /**
   * Remove an extended attribute
   *
   * @param ino the inode number
   * @param name of the extended attribute
   */
  virtual folly::Future<folly::Unit> removexattr(fuse_ino_t ino,
                                                 folly::StringPiece name);

  /**
   * Check file access permissions
   *
   * This will be called for the access() system call.  If the
   * 'default_permissions' mount option is given, this method is not
   * called.
   *
   * This method is not called under Linux kernel versions 2.4.x
   *
   * Introduced in version 2.5
   *
   * @param ino the inode number
   * @param mask requested access mode
   */
  virtual folly::Future<folly::Unit> access(fuse_ino_t ino, int mask);

  struct Create {
    fuse_entry_param entry;
    FileHandle* fh;
  };

  /**
   * Create and open a file
   *
   * If the file does not exist, first create it with the specified
   * mode, and then open it.
   *
   * Open flags (with the exception of O_NOCTTY) are available in
   * fi->flags.
   *
   * If this method is not implemented or under Linux kernel
   * versions earlier than 2.6.15, the mknod() and open() methods
   * will be called instead.
   *
   * Introduced in version 2.5
   *
   * @param parent inode number of the parent directory
   * @param name to create
   * @param mode file type and mode with which to create the new file
   */
  virtual folly::Future<Create>
  create(fuse_ino_t parent, PathComponentPiece name, mode_t mode, int flags);

  /**
   * Map block index within file to block index within device
   *
   * Note: This makes sense only for block device backed filesystems
   * mounted with the 'blkdev' option
   *
   * Introduced in version 2.6
   *
   * @param ino the inode number
   * @param blocksize unit of block index
   * @param idx block index within file
   */
  virtual folly::Future<uint64_t> bmap(fuse_ino_t ino,
                                       size_t blocksize,
                                       uint64_t idx);
};

}
}
}
