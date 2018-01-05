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

#include <folly/Portability.h>
#include <folly/SharedMutex.h>
#include <folly/Synchronized.h>
#include <folly/ThreadLocal.h>
#include <folly/experimental/logging/Logger.h>
#include <folly/futures/Future.h>
#include <folly/futures/Promise.h>
#include <chrono>
#include <memory>
#include <mutex>
#include <shared_mutex>
#include "eden/fs/fuse/EdenStats.h"
#include "eden/fs/fuse/FuseChannel.h"
#include "eden/fs/inodes/InodePtrFwd.h"
#include "eden/fs/journal/JournalDelta.h"
#include "eden/fs/model/ParentCommits.h"
#include "eden/fs/service/gen-cpp2/eden_types.h"
#include "eden/fs/utils/PathFuncs.h"

namespace folly {
class EventBase;
class File;

template <typename T>
class Future;
} // namespace folly

namespace facebook {
namespace eden {
namespace fusell {
class FuseChannel;
class MountPoint;
} // namespace fusell

class BindMount;
class CheckoutConflict;
class ClientConfig;
class Clock;
class DiffContext;
class EdenDispatcher;
class InodeDiffCallback;
class InodeMap;
class ObjectStore;
class Overlay;
class Journal;
class Tree;
class UnboundedQueueThreadPool;

class RenameLock;
class SharedRenameLock;

/**
 * Represents types of keys for some fb303 counters.
 */
enum class CounterName {
  /**
   * Represents count of loaded inodes in the current mount.
   */
  LOADED,
  /**
   * Represents count of unloaded inodes in the current mount.
   */
  UNLOADED
};

/**
 * EdenMount contains all of the data about a specific eden mount point.
 *
 * This contains:
 * - The fusell::MountPoint object which manages our FUSE interactions with the
 *   kernel.
 * - The ObjectStore object used for retreiving/storing object data.
 * - The Overlay object used for storing local changes (that have not been
 *   committed/snapshotted yet).
 */
class EdenMount {
 public:
  /**
   * Create a shared_ptr to an EdenMount.
   *
   * Create an EdenMount instance and asynchronously initialize it. We use
   * an EdenMountDeleter.
   */
  static folly::Future<std::shared_ptr<EdenMount>> create(
      std::unique_ptr<ClientConfig> config,
      std::unique_ptr<ObjectStore> objectStore,
      AbsolutePathPiece socketPath,
      fusell::ThreadLocalEdenStats* globalStats,
      std::shared_ptr<Clock> clock);

  /**
   * Destroy the EdenMount.
   *
   * This method generally does not need to be invoked directly, and will
   * instead be invoked automatically by the shared_ptr<EdenMount> returned by
   * create(), once it becomes unreferenced.
   *
   * If the EdenMount has not already been explicitly shutdown(), destroy()
   * will trigger the shutdown().  destroy() blocks until the shutdown is
   * complete, so it is advisable for callers to callers to explicitly trigger
   * shutdown() themselves if they want to ensure that the shared_ptr
   * destruction will not block on this operation.
   */
  void destroy();

  /**
   * Shutdown the EdenMount.
   *
   * This should be called *after* the FUSE mount point has been unmounted from
   * the kernel.
   *
   * This cleans up the in-memory data associated with the EdenMount, and waits
   * for all outstanding InodeBase objects to become unreferenced and be
   * destroyed.
   */
  folly::Future<folly::Unit> shutdown();

  /**
   * Get the FUSE channel for this mount point.
   *
   * This should only be called after the mount point has been successfully
   * started.  (It is the caller's responsibility to perform proper
   * synchronization here with the mount start operation.  This method provides
   * no internal synchronization of its own.)
   */
  fusell::FuseChannel* getFuseChannel() const;

  /**
   * Return the path to the mount point.
   */
  const AbsolutePath& getPath() const;

  /**
   * Get the commit IDs of the working directory's parent commit(s).
   */
  ParentCommits getParentCommits() const {
    return parentInfo_.rlock()->parents;
  }

  /*
   * Return bind mounts that are applied for this mount. These are based on the
   * state of the ClientConfig when this EdenMount was created.
   */
  const std::vector<BindMount>& getBindMounts() const;

  /**
   * Return the ObjectStore used by this mount point.
   *
   * The ObjectStore is guaranteed to be valid for the lifetime of the
   * EdenMount.
   */
  ObjectStore* getObjectStore() const {
    return objectStore_.get();
  }

  /**
   * Return the EdenDispatcher used for this mount.
   */
  EdenDispatcher* getDispatcher() const {
    return dispatcher_.get();
  }

  /**
   * Return the InodeMap for this mount.
   */
  InodeMap* getInodeMap() const {
    return inodeMap_.get();
  }

  const std::shared_ptr<Overlay>& getOverlay() const {
    return overlay_;
  }

  Journal& getJournal() {
    return journal_;
  }

  uint64_t getMountGeneration() const {
    return mountGeneration_;
  }

  const ClientConfig* getConfig() const {
    return config_.get();
  }

  /**
   * Returns the EventBase for this mount.
   */
  folly::EventBase* getEventBase() const {
    return eventBase_;
  }

  /**
   * Returns the server's thread pool.
   */
  const std::shared_ptr<UnboundedQueueThreadPool>& getThreadPool() const {
    return threadPool_;
  }

  /**
   * Returns the Clock with which this mount was configured.
   */
  Clock& getClock() {
    return *clock_;
  }

  /** Get the TreeInode for the root of the mount. */
  TreeInodePtr getRootInode() const;

  /** Get the inode number for the .eden dir */
  fusell::InodeNumber getDotEdenInodeNumber() const;

  /** Convenience method for getting the Tree for the root of the mount. */
  std::shared_ptr<const Tree> getRootTree() const;
  folly::Future<std::shared_ptr<const Tree>> getRootTreeFuture() const;

  /**
   * Look up the Inode object for the specified path.
   *
   * This may fail with an InodeError containing ENOENT if the path does not
   * exist, or ENOTDIR if one of the intermediate components along the path is
   * not a directory.
   *
   * This may also fail with other exceptions if something else goes wrong
   * besides the path being invalid (for instance, an error loading data from
   * the ObjectStore).
   */
  folly::Future<InodePtr> getInode(RelativePathPiece path) const;

  /**
   * A blocking version of getInode().
   *
   * @return the InodeBase for the specified path or throws a std::system_error
   *     with ENOENT.
   *
   * TODO: We should switch all callers to use the Future-base API, and remove
   * the blocking API.
   */
  InodePtr getInodeBlocking(RelativePathPiece path) const;

  /**
   * Syntactic sugar for getInode().get().asTreePtr()
   *
   * TODO: We should switch all callers to use the Future-base API, and remove
   * the blocking API.
   */
  TreeInodePtr getTreeInodeBlocking(RelativePathPiece path) const;

  /**
   * Syntactic sugar for getInode().get().asFilePtr()
   *
   * TODO: We should switch all callers to use the Future-base API, and remove
   * the blocking API.
   */
  FileInodePtr getFileInodeBlocking(RelativePathPiece path) const;

  /**
   * Chases (to bounded depth) and returns the final non-symlink in the
   * (possibly 0-length) chain of symlinks rooted at pInode.  Specifically:
   * If pInode is a file or directory, it is immediately returned.
   * If pInode is a symlink, the chain rooted at it chased down until
   * one of the following conditions:
   * 1) an entity outside this mount is encountered => error (EXDEV);
   * 2) an non-symlink item under this mount is found => this item is returned;
   * 3) a maximum depth is exceeded => error (ELOOP).
   * 4) absolute path entity is encountered => error (EPERM).
   * 5) the input inode refers to an unlinked inode => error (ENOENT).
   * 6) a symlink points to a non-existing entity => error (ENOENT)
   * NOTE: a loop in the chain is handled by max depth length logic.
   */
  folly::Future<InodePtr> resolveSymlink(InodePtr pInode) const;

  /**
   * Check out the specified commit.
   */
  folly::Future<std::vector<CheckoutConflict>> checkout(
      Hash snapshotHash,
      CheckoutMode checkoutMode = CheckoutMode::NORMAL);

  /**
   * This version of diff is primarily intended for testing.
   * Use diff(InodeDiffCallback* callback, bool listIgnored) instead.
   * The caller must ensure that the DiffContext object ctsPtr points to
   * exists at least until the returned Future completes.
   */
  folly::Future<folly::Unit> diff(const DiffContext* ctxPtr) const;

  /**
   * Compute differences between the current commit and the working directory
   * state.
   *
   * @param callback This callback will be invoked as differences are found.
   *     Note that the callback methods may be invoked simultaneously from
   *     multiple different threads, and the callback is responsible for
   *     performing synchronization (if it is needed).
   * @param listIgnored Whether or not to inform the callback of ignored files.
   *     When listIgnored is set to false can speed up the diff computation, as
   *     the code does not need to descend into ignored directories at all.
   *
   * @return Returns a folly::Future that will be fulfilled when the diff
   *     operation is complete.  This is marked FOLLY_NODISCARD to
   *     make sure callers do not forget to wait for the operation to complete.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> diff(
      InodeDiffCallback* callback,
      bool listIgnored = false) const;

  /**
   * Compute the differences between the trees in the specified commits.
   * This does not care about the working copy aside from using it as the
   * source of the backing store for the commits.
   *
   * @param callback This callback will be invoked as differences are found.
   *     Note that the callback methods may be invoked simultaneously from
   *     multiple different threads, and the callback is responsible for
   *     performing synchronization (if it is needed).
   *
   * @return Returns a folly::Future that will be fulfilled when the diff
   *     operation is complete.  This is marked FOLLY_NODISCARD to
   *     make sure callers do not forget to wait for the operation to complete.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit>
  diffRevisions(InodeDiffCallback* callback, Hash fromHash, Hash toHash);

  /**
   * Reset the state to point to the specified parent commit(s), without
   * modifying the working directory contents at all.
   */
  void resetParents(const ParentCommits& parents);

  /**
   * Reset the state to point to the specified parent commit, without
   * modifying the working directory contents at all.
   *
   * This is a small wrapper around resetParents() for when the code knows at
   * compile time that it will only ever have a single parent commit on this
   * code path.
   */
  void resetParent(const Hash& parent);

  /**
   * Acquire the rename lock in exclusive mode.
   */
  RenameLock acquireRenameLock();

  /**
   * Acquire the rename lock in shared mode.
   */
  SharedRenameLock acquireSharedRenameLock();

  const AbsolutePath& getSocketPath() const;

  /**
   * Returns a pointer to a stats instance associated with this mountpoint.
   * Today this is the global stats instance, but in the future it will be
   * a mount point specific instance.
   */
  fusell::ThreadLocalEdenStats* getStats() const;

  folly::Logger& getStraceLogger() {
    return straceLogger_;
  }

  /**
   * Returns the last checkout time in the Eden mount.
   */
  struct timespec getLastCheckoutTime();

  /**
   * Set the last checkout time.
   *
   * This is intended primarily for use in test code.
   */
  void setLastCheckoutTime(std::chrono::system_clock::time_point time);

  /**
   * Returns the key value to an fb303 counter.
   */
  std::string getCounterName(CounterName name);

  struct ParentInfo {
    ParentCommits parents;
  };

  /**
   * Mounts the filesystem in the VFS and spawns worker threads to
   * dispatch the fuse session.
   *
   * Returns a Future that will complete as soon as the filesystem has been
   * successfully mounted, or as soon as the mount fails (state transitions
   * to RUNNING or FUSE_ERROR).
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> startFuse(
      folly::EventBase* eventBase,
      std::shared_ptr<UnboundedQueueThreadPool> threadPool);

  /**
   * Obtains a future that will complete once the state transitions to
   * FUSE_DONE and the thread pool has been joined.
   */
  FOLLY_NODISCARD folly::Future<folly::File> getFuseCompletionFuture();

  uid_t getUid() const {
    return uid_;
  }

  gid_t getGid() const {
    return gid_;
  }

  /**
   * Indicate that the mount point has been successfully started.
   *
   * This function should only be invoked by the Dispatcher class.
   */
  void mountStarted();

  /**
   * Return a new stat structure that has been minimally initialized with
   * data for this mount point.
   *
   * The caller must still initialize all file-specific data (inode number,
   * file mode, size, timestamps, link count, etc).
   */
  struct stat initStatData() const;

  /**
   * mount any configured bind mounts.
   * This requires that the filesystem already be mounted, and must not
   * be called in the context of a fuseWorkerThread().
   */
  void performBindMounts();

 private:
  friend class RenameLock;
  friend class SharedRenameLock;

  /**
   * The current running state of the EdenMount.
   *
   * For now this primarily tracks the status of the shutdown process.
   * In the future we may want to add other states to also track the status of
   * the actual mount point in the kernel.  (e.g., a "STARTING" state before
   * RUNNING for when the kernel mount point has not been fully set up yet, and
   * an "UNMOUNTING" state if we have requested the kernel to unmount the mount
   * point and that has not completed yet.  UNMOUNTING would occur between
   * RUNNING and SHUT_DOWN.)  One possible downside of tracking
   * STARTING/UNMOUNTING is that not every EdenMount object actually has a FUSE
   * mount.  During unit tests we create EdenMount objects without ever
   * actually mounting them in the kernel.
   */
  enum class State : uint32_t {
    /**
     * Freshly created.
     */
    UNINITIALIZED,

    /**
     * Starting to mount fuse.
     */
    STARTING,

    /**
     * The EdenMount is running normally.
     */
    RUNNING,

    /**
     * Encountered an error while starting fuse mount.
     */
    FUSE_ERROR,

    /**
     * Fuse session completed and the thread pools are stopping or stopped.
     * fuseCompletionPromise_ is about to be fulfilled, which will cause
     * someone else to call EdenMount::shutdown().
     */
    FUSE_DONE,

    /**
     * EdenMount::shutdown() has been called, but it is not complete yet.
     */
    SHUTTING_DOWN,

    /**
     * EdenMount::shutdown() has completed, but there are still outstanding
     * references so EdenMount::destroy() has not been called yet.
     *
     * When EdenMount::destroy() is called the object can be destroyed
     * immediately.
     */
    SHUT_DOWN,

    /**
     * EdenMount::destroy() has been called, but the shutdown is not complete
     * yet.  There are no remaining references to the EdenMount at this point,
     * so when the shutdown completes it will be automatically destroyed.
     */
    DESTROYING
  };

  /**
   * Recursive method used for resolveSymlink() implementation
   */
  folly::Future<InodePtr>
  resolveSymlinkImpl(InodePtr pInode, RelativePath&& path, size_t depth) const;

  /**
   * Attempt to transition from expected -> newState.
   * If the current state is expected then the state is set to newState
   * and returns boolean.
   * Otherwise the current state is left untouched and returns false.
   */
  bool doStateTransition(State expected, State newState);

  EdenMount(
      std::unique_ptr<ClientConfig> config,
      std::unique_ptr<ObjectStore> objectStore,
      AbsolutePathPiece socketPath,
      fusell::ThreadLocalEdenStats* globalStats,
      std::shared_ptr<Clock> clock);

  // Forbidden copy constructor and assignment operator
  EdenMount(EdenMount const&) = delete;
  EdenMount& operator=(EdenMount const&) = delete;

  /**
   * Asynchronous EdenMount initialization - post instantiation.
   */
  folly::Future<folly::Unit> initialize();

  folly::Future<TreeInodePtr> createRootInode(
      const ParentCommits& parentCommits);
  folly::Future<folly::Unit> setupDotEden(TreeInodePtr root);
  folly::Future<folly::Unit> shutdownImpl();

  struct timespec getCurrentCheckoutTime();

  /**
   * Private destructor.
   *
   * This should not be invoked by callers directly.  Use the destroy() method
   * above (or the EdenMountDeleter if you plan to store the EdenMount in a
   * std::unique_ptr or std::shared_ptr).
   */
  ~EdenMount();

  static constexpr int kMaxSymlinkChainDepth = 40; // max depth of symlink chain

  /**
   * The stats instance associated with this mount point.
   * This is just a reference to a global stats instance today, but we'd
   * like to make this its own child instance that aggregates up into
   * the global instance in the future.
   */
  fusell::ThreadLocalEdenStats* globalEdenStats_{nullptr};

  std::unique_ptr<ClientConfig> config_;
  std::unique_ptr<InodeMap> inodeMap_;
  std::unique_ptr<EdenDispatcher> dispatcher_;
  std::unique_ptr<ObjectStore> objectStore_;
  std::shared_ptr<Overlay> overlay_;
  fusell::InodeNumber dotEdenInodeNumber_{0};

  /**
   * A mutex around all name-changing operations in this mount point.
   *
   * This includes rename() operations as well as unlink() and rmdir().
   * Any operation that modifies an existing InodeBase's location_ data must
   * hold the rename lock.
   */
  folly::SharedMutex renameMutex_;

  /**
   * The IDs of the parent commit(s) of the working directory.
   *
   * In most circumstances there will only be a single parent, but there
   * will be two parents when in the middle of resolving a merge conflict.
   */

  folly::Synchronized<ParentInfo> parentInfo_;

  /*
   * Note that this config will not be updated if the user modifies the
   * underlying config files after the ClientConfig was created.
   */
  const std::vector<BindMount> bindMounts_;

  Journal journal_;

  /**
   * A number to uniquely identify this particular incarnation of this mount.
   * We use bits from the process id and the time at which we were mounted.
   */
  const uint64_t mountGeneration_;

  /**
   * The path to the unix socket that can be used to address us via thrift
   */
  AbsolutePath socketPath_;

  /**
   * A log category for logging strace-events for this mount point.
   *
   * All FUSE operations to this mount point will get logged to this category.
   * The category name is of the following form: "eden.strace.<mount_path>"
   */
  folly::Logger straceLogger_;

  /**
   * The timestamp of the last time that a checkout operation was performed in
   * this mount.  This is used to initialize the timestamps of newly loaded
   * inodes.  (Since the file contents might have logically been update by the
   * checkout operation.)
   *
   * We store this as a struct timespec rather than a std::chrono::time_point
   * since this is primarily used by FUSE APIs which need a timespec.
   *
   * This is managed with its own Synchronized lock separate from other state
   * since it needs to be accessed when constructing inodes.  This is a very
   * low level lock in our lock ordering hierarchy: No other locks should be
   * acquired while holding this lock.
   */
  folly::Synchronized<struct timespec> lastCheckoutTime_;

  /**
   * The current state of the mount point.
   */
  std::atomic<State> state_{State::UNINITIALIZED};

  /**
   * A promise associated with the future returned from EdenMount::startFuse()
   * that completes when the state transitions to RUNNING or FUSE_ERROR.
   */
  folly::Promise<folly::Unit> initFusePromise_;

  /**
   * A promise associated with the future returned from
   * EdenMount::getFuseCompletionFuture() that completes when the state
   * transitions to FUSE_DONE.
   * The future yields the underlying fuseDevice descriptor; it can
   * be passed on during graceful restart or simply closed if we're
   * unmounting and shutting down completely.  In the unmount scenario
   * the device should be closed prior to calling EdenMount::shutdown()
   * so that the subsequent privilegedFuseUnmount() call won't block
   * waiting on us for a response.
   */
  folly::Promise<folly::File> fuseCompletionPromise_;

  AbsolutePath const path_; // the path where this MountPoint is mounted

  /**
   * uid and gid that we'll set as the owners in the stat information
   * returned via initStatData().
   */
  uid_t uid_;
  gid_t gid_;

  /**
   * The associated fuse channel to the kernel.
   */
  std::unique_ptr<fusell::FuseChannel> channel_;

  /**
   * The main eventBase of the program; this is used to join and dispatch
   * promises when transitioning to FUSE_DONE.
   */
  folly::EventBase* eventBase_{nullptr};

  /**
   * The server's thread pool passed into startFuse().  Notably, it is always
   * safe to queue work into this pool.
   */
  std::shared_ptr<UnboundedQueueThreadPool> threadPool_;

  std::shared_ptr<Clock> clock_;
};

/**
 * RenameLock is a holder for an EdenMount's rename mutex.
 *
 * This is primarily useful so it can be forward declared easily,
 * but it also provides a helper method to ensure that it is currently holding
 * a lock on the desired mount.
 */
class RenameLock : public std::unique_lock<folly::SharedMutex> {
 public:
  RenameLock() {}
  explicit RenameLock(EdenMount* mount)
      : std::unique_lock<folly::SharedMutex>{mount->renameMutex_} {}

  bool isHeld(EdenMount* mount) const {
    return owns_lock() && (mutex() == &mount->renameMutex_);
  }
};

/**
 * SharedRenameLock is a holder for an EdenMount's rename mutex in shared mode.
 */
class SharedRenameLock : public std::shared_lock<folly::SharedMutex> {
 public:
  explicit SharedRenameLock(EdenMount* mount)
      : std::shared_lock<folly::SharedMutex>{mount->renameMutex_} {}

  bool isHeld(EdenMount* mount) const {
    return owns_lock() && (mutex() == &mount->renameMutex_);
  }
};

/**
 * EdenMountDeleter acts as a deleter argument for std::shared_ptr or
 * std::unique_ptr.
 */
class EdenMountDeleter {
 public:
  void operator()(EdenMount* mount) {
    mount->destroy();
  }
};
} // namespace eden
} // namespace facebook
