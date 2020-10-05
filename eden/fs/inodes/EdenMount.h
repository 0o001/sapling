/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/Portability.h>
#include <folly/SharedMutex.h>
#include <folly/Synchronized.h>
#include <folly/ThreadLocal.h>
#include <folly/futures/Future.h>
#include <folly/futures/Promise.h>
#include <folly/futures/SharedPromise.h>
#include <folly/logging/Logger.h>
#include <chrono>
#include <memory>
#include <mutex>
#include <optional>
#include <shared_mutex>
#include <stdexcept>
#include "eden/fs/inodes/CacheHint.h"
#include "eden/fs/inodes/InodeNumber.h"
#include "eden/fs/inodes/InodePtrFwd.h"
#include "eden/fs/inodes/Overlay.h"
#include "eden/fs/journal/Journal.h"
#include "eden/fs/model/ParentCommits.h"
#include "eden/fs/service/gen-cpp2/eden_types.h"
#include "eden/fs/store/BlobAccess.h"
#include "eden/fs/takeover/TakeoverData.h"
#include "eden/fs/utils/PathFuncs.h"

#ifndef _WIN32
#include "eden/fs/fuse/Dispatcher.h"
#include "eden/fs/fuse/FuseChannel.h"
#include "eden/fs/inodes/OverlayFileAccess.h"
#else
#include "eden/fs/prjfs/PrjfsChannel.h"
#endif

DECLARE_string(edenfsctlPath);

namespace apache {
namespace thrift {
class ResponseChannelRequest;
}
} // namespace apache

namespace folly {
class EventBase;
class File;

template <typename T>
class Future;
} // namespace folly

namespace facebook {
namespace eden {

class BindMount;
class BlobCache;
class CheckoutConfig;
class CheckoutConflict;
class Clock;
class DiffContext;
class EdenDispatcher;
class FuseChannel;
class FuseDeviceUnmountedDuringInitialization;
class DiffCallback;
class InodeMap;
class MountPoint;
struct InodeMetadata;
template <typename T>
class InodeTable;
using InodeMetadataTable = InodeTable<InodeMetadata>;
class ObjectStore;
class Overlay;
class OverlayFileAccess;
class ServerState;
class Tree;
class TreePrefetchLease;
class UnboundedQueueExecutor;
struct FileMetadata;

class RenameLock;
class SharedRenameLock;

/**
 * Represents types of keys for some fb303 counters.
 */
enum class CounterName {
  /**
   * Represents count of loaded inodes in the current mount.
   */
  INODEMAP_LOADED,
  /**
   * Represents count of unloaded inodes in the current mount.
   */
  INODEMAP_UNLOADED,
  /**
   * Represents the amount of memory used by deltas in the change log
   */
  JOURNAL_MEMORY,
  /**
   * Represents the number of entries in the change log
   */
  JOURNAL_ENTRIES,
  /**
   * Represents the duration of the journal in seconds end to end
   */
  JOURNAL_DURATION,
  /**
   * Represents the maximum deltas iterated over in the Journal's forEachDelta
   */
  JOURNAL_MAX_FILES_ACCUMULATED
};

/**
 * Contains the uid and gid of the owner of the files in the mount
 */
struct Owner {
  uid_t uid;
  gid_t gid;
};

/**
 * Durations of the various stages of checkout.
 */
struct CheckoutTimes {
  using duration = std::chrono::steady_clock::duration;
  duration didAcquireParentsLock{};
  duration didLookupTrees{};
  duration didDiff{};
  duration didAcquireRenameLock{};
  duration didCheckout{};
  duration didFinish{};
};

struct CheckoutResult {
  std::vector<CheckoutConflict> conflicts;
  CheckoutTimes times;
};

/**
 * EdenMount contains all of the data about a specific eden mount point.
 *
 * This contains:
 * - The MountPoint object which manages our FUSE interactions with the kernel.
 * - The ObjectStore object used for retreiving/storing object data.
 * - The Overlay object used for storing local changes (that have not been
 *   committed/snapshotted yet).
 */
class EdenMount {
 public:
  using State = MountState;

  /**
   * Create a shared_ptr to an EdenMount.
   *
   * The caller must call initialize() after creating the EdenMount to load data
   * required to access the mount's inodes.  No inode-related methods may be
   * called on the EdenMount until initialize() has successfully completed.
   */
  static std::shared_ptr<EdenMount> create(
      std::unique_ptr<CheckoutConfig> config,
      std::shared_ptr<ObjectStore> objectStore,
      std::shared_ptr<BlobCache> blobCache,
      std::shared_ptr<ServerState> serverState,
      std::unique_ptr<Journal> journal);

  /**
   * Asynchronous EdenMount initialization - post instantiation.
   *
   * If takeover data is specified, it is used to initialize the inode map.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> initialize(
      OverlayChecker::ProgressCallback&& progressCallback = [](auto) {},
      const std::optional<SerializedInodeMap>& takeover = std::nullopt);

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
   * This should be called *after* calling unmount() (i.e. after the FUSE mount
   * point has been unmounted from the kernel).
   *
   * This cleans up the in-memory data associated with the EdenMount, and waits
   * for all outstanding InodeBase objects to become unreferenced and be
   * destroyed.
   *
   * If doTakeover is true, this function will return populated
   * SerializedFileHandleMap and SerializedInodeMap instances generated by
   * calling FileHandleMap::serializeMap() and InodeMap::shutdown.
   *
   * If doTakeover is false, this function will return default-constructed
   * SerializedFileHandleMap and SerializedInodeMap instances.
   */
  folly::SemiFuture<SerializedInodeMap> shutdown(
      bool doTakeover,
      bool allowFuseNotStarted = false);

  /**
   * Call the umount(2) syscall to tell the kernel to remove this filesystem.
   *
   * After umount(2) succeeds, the following operations happen independently and
   * concurrently:
   *
   * * The future returned by unmount() is fulfilled successfully.
   * * The future returned by getChannelCompletionFuture() is fulfilled.
   *
   * If startChannel() is in progress, unmount() can cancel startChannel().
   *
   * If startChannel() is in progress, unmount() might wait for startChannel()
   * to finish before calling umount(2).
   *
   * If neither startChannel() nor takeoverFuse() has been called, unmount()
   * finishes successfully without calling umount(2). Thereafter, startChannel()
   * and takeoverFuse() will both fail with an EdenMountCancelled exception.
   *
   * unmount() is idempotent: If unmount() has already been called, this
   * function immediately returns a Future which will complete at the same time
   * the original call to unmount() completes.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> unmount();

  /**
   * Get the current state of this mount.
   *
   * Note that the state may be changed by another thread immediately after this
   * method is called, so this method should primarily only be used for
   * debugging & diagnostics.
   */
  State getState() const {
    return state_.load(std::memory_order_acquire);
  }

  /**
   * Check if inode operations can be performed on this EdenMount.
   *
   * This returns false for mounts that are still initializing and do not have
   * their root inode loaded yet. This also returns false for mounts that are
   * shutting down.
   */
  bool isSafeForInodeAccess() const {
    auto state = getState();
    return !(
        state == State::UNINITIALIZED || state == State::INITIALIZING ||
        state == State::SHUTTING_DOWN);
  }

  /**
   * Get the FUSE/Prjfs channel for this mount point.
   *
   * This should only be called after the mount point has been successfully
   * started.  (It is the caller's responsibility to perform proper
   * synchronization here with the mount start operation.  This method provides
   * no internal synchronization of its own.)
   */
#ifdef _WIN32
  PrjfsChannel* getPrjfsChannel() const;
#else
  FuseChannel* getFuseChannel() const;
#endif

  ProcessAccessLog& getProcessAccessLog() const {
#ifdef _WIN32
    return getPrjfsChannel()->getProcessAccessLog();
#else
    return getFuseChannel()->getProcessAccessLog();
#endif
  }

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
   * Return Eden's blob cache.
   *
   * It is guaranteed to be valid for the lifetime of the EdenMount.
   */
  BlobCache* getBlobCache() const {
    return blobCache_.get();
  }

  /**
   * Return the BlobAccess used by this mount point.
   *
   * The BlobAccess is guaranteed to be valid for the lifetime of the EdenMount.
   */
  BlobAccess* getBlobAccess() {
    return &blobAccess_;
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

  /**
   * Return the Overlay for this mount.
   */
  Overlay* getOverlay() const {
    return overlay_.get();
  }

#ifndef _WIN32
  OverlayFileAccess* getOverlayFileAccess() {
    return &overlayFileAccess_;
  }

#endif // !_WIN32

  InodeMetadataTable* getInodeMetadataTable() const;

  /**
   * Return the Journal used by this mount point.
   *
   * The Journal is guaranteed to be valid for the lifetime of the EdenMount.
   */
  Journal& getJournal() {
    return *journal_;
  }

  uint64_t getMountGeneration() const {
    return mountGeneration_;
  }

  const CheckoutConfig* getConfig() const {
    return config_.get();
  }

  /**
   * Returns the server's thread pool.
   */
  const std::shared_ptr<UnboundedQueueExecutor>& getThreadPool() const;

  /**
   * Returns the Clock with which this mount was configured.
   */
  const Clock& getClock() const {
    return *clock_;
  }

  /** Get the TreeInode for the root of the mount. */
  TreeInodePtr getRootInode() const;

#ifndef _WIN32
  /**
   * Get the inode number for the .eden dir.  Returns an empty InodeNumber
   * prior to the .eden directory being set up.
   */
  InodeNumber getDotEdenInodeNumber() const;
#endif // !_WIN32

  /** Convenience method for getting the Tree for the root of the mount. */
  folly::Future<std::shared_ptr<const Tree>> getRootTree() const;

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
  folly::Future<InodePtr> getInode(
      RelativePathPiece path,
      ObjectFetchContext& context) const;

  /**
   * Resolves symlinks and loads file contents from the Inode at the given path.
   * This loads the entire file contents into memory, so this can be expensive
   * for large files.
   *
   * The fetchContext object must remain valid until the future is completed.
   */
  folly::Future<std::string> loadFileContentsFromPath(
      ObjectFetchContext& fetchContext,
      RelativePathPiece path,
      CacheHint cacheHint = CacheHint::LikelyNeededAgain) const;

  /**
   * Resolves symlinks and loads file contents. This loads the entire file
   * contents into memory, so this can be expensive for large files.
   *
   * The fetchContext object must remain valid until the future is completed.
   *
   * TODO: add maxSize parameter to cause the command to fail if the file is
   * over a certain size.
   */
  folly::Future<std::string> loadFileContents(
      ObjectFetchContext& fetchContext,
      InodePtr fileInodePtr,
      CacheHint cacheHint = CacheHint::LikelyNeededAgain) const;

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
  folly::Future<InodePtr> resolveSymlink(
      ObjectFetchContext& fetchContext,
      InodePtr pInode,
      CacheHint cacheHint = CacheHint::LikelyNeededAgain) const;

  /**
   * Check out the specified commit.
   */
  folly::Future<CheckoutResult> checkout(
      Hash snapshotHash,
      std::optional<pid_t> clientPid,
      folly::StringPiece thriftMethodCaller,
      CheckoutMode checkoutMode = CheckoutMode::NORMAL);

  /**
   * Chown the repository to the given uid and gid
   */
  folly::Future<folly::Unit> chown(uid_t uid, gid_t gid);

  /**
   * Compute differences between the current commit and the working directory
   * state.
   *
   * @param listIgnored Whether or not to inform the callback of ignored files.
   *     When listIgnored is set to false can speed up the diff computation, as
   *     the code does not need to descend into ignored directories at all.
   * @param enforceCurrentParent Whether or not to return an error if the
   *     specified commitHash does not match the actual current working
   *     directory parent.  If this is false the code will still compute a diff
   *     against the specified commitHash even the working directory parent
   *     points elsewhere, or when a checkout is currently in progress.
   * @param request This ResposeChannelRequest is passed from the ServiceHandler
   *     and is used to check if the request is still active, because if the
   *     request is no longer active we will cancel this diff operation.
   *
   * @return Returns a folly::Future that will be fulfilled when the diff
   *     operation is complete.  This is marked FOLLY_NODISCARD to
   *     make sure callers do not forget to wait for the operation to complete.
   */
  FOLLY_NODISCARD folly::Future<std::unique_ptr<ScmStatus>> diff(
      Hash commitHash,
      bool listIgnored = false,
      bool enforceCurrentParent = true,
      apache::thrift::ResponseChannelRequest* FOLLY_NULLABLE request = nullptr);

  /**
   * This version of diff is primarily intended for testing.
   * Use diff(DiffCallback* callback, bool listIgnored) instead.
   * The caller must ensure that the DiffContext object ctsPtr points to
   * exists at least until the returned Future completes.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> diff(
      DiffContext* ctxPtr,
      Hash commitHash) const;

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

  /**
   * Returns a pointer to a stats instance associated with this mountpoint.
   * Today this is the global stats instance, but in the future it will be
   * a mount point specific instance.
   */
  EdenStats* getStats() const;

  const folly::Logger& getStraceLogger() const {
    return straceLogger_;
  }

  const std::shared_ptr<ServerState>& getServerState() const {
    return serverState_;
  }

  /**
   * Returns the last checkout time in the Eden mount.
   */
  struct timespec getLastCheckoutTime() const;

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
   *
   * If unmount() is called before startChannel() is called, then startChannel()
   * does the following:
   *
   * * startChannel() does not attempt to mount the filesystem
   * * The returned Future is fulfilled with an EdenMountCancelled exception
   *
   * If unmount() is called while startChannel() is in progress, then
   * startChannel() does the following:
   *
   * * The filesystem is unmounted (if it was mounted)
   * * The returned Future is fulfilled with an
   *   FuseDeviceUnmountedDuringInitialization exception
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> startChannel(bool readOnly);

#ifndef _WIN32
  /**
   * Take over a FUSE channel for an existing mount point.
   *
   * This spins up worker threads to service the existing FUSE channel and
   * returns immediately, or throws an exception on error.
   *
   * If unmount() is called before takeoverFuse() is called, then takeoverFuse()
   * throws an EdenMountCancelled exception.
   */
  void takeoverFuse(FuseChannelData takeoverData);
#endif

  /**
   * Obtains a future that will complete once the channel has wound down.
   *
   * This method may be called at any time, but the returned future will only be
   * fulfilled if startChannel() completes successfully.  If startChannel()
   * fails or is never called, the future returned by
   * getChannelCompletionFuture() will never complete.
   */
  FOLLY_NODISCARD folly::Future<TakeoverData::MountInfo>
  getChannelCompletionFuture();

  Owner getOwner() const {
    return *owner_.rlock();
  }

  void setOwner(uid_t uid, gid_t gid) {
    auto owner = owner_.wlock();
    owner->uid = uid;
    owner->gid = gid;
  }

  /**
   * Return a new stat structure that has been minimally initialized with
   * data for this mount point.
   *
   * The caller must still initialize all file-specific data (inode number,
   * file mode, size, timestamps, link count, etc).
   */
  struct stat initStatData() const;

  /**
   * Given a mode_t, return an initial InodeMetadata.  All timestamps are set
   * to the last checkout time and uid and gid are set to the creator of the
   * mount.
   */
  struct InodeMetadata getInitialInodeMetadata(mode_t mode) const;

  /**
   * mount any configured bind mounts.
   * This requires that the filesystem already be mounted, and must not
   * be called in the context of a fuseWorkerThread().
   */
  FOLLY_NODISCARD folly::SemiFuture<folly::Unit> performBindMounts();

  FOLLY_NODISCARD folly::Future<folly::Unit> addBindMount(
      RelativePathPiece repoPath,
      AbsolutePathPiece targetPath);
  FOLLY_NODISCARD folly::Future<folly::Unit> removeBindMount(
      RelativePathPiece repoPath);

  /**
   * Ensures the path `fromRoot` is a directory. If it is not, then it creates
   * subdirectories until it is. If creating a subdirectory fails, it throws an
   * exception.
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> ensureDirectoryExists(
      RelativePathPiece fromRoot);

  /**
   * Request to start a new tree prefetch.
   *
   * Returns a new TreePrefetchLease if you can start a new prefetch, or
   * std::nullopt if there are too many prefetches already in progress and a new
   * one should not be started.  If a TreePrefetchLease object is returned the
   * caller should hold onto it until the prefetch is complete.  When the
   * TreePrefetchLease is destroyed this will inform the EdenMount that the
   * prefetch has finished.
   */
  FOLLY_NODISCARD std::optional<TreePrefetchLease> tryStartTreePrefetch(
      TreeInodePtr treeInode,
      ObjectFetchContext& context);

 private:
  friend class RenameLock;
  friend class SharedRenameLock;
  class JournalDiffCallback;

  /**
   * Recursive method used for resolveSymlink() implementation
   */
  folly::Future<InodePtr> resolveSymlinkImpl(
      ObjectFetchContext& fetchContext,
      InodePtr pInode,
      RelativePath&& path,
      size_t depth,
      CacheHint cacheHint) const;

  /**
   * Attempt to transition from expected -> newState.
   * If the current state is expected then the state is set to newState
   * and returns boolean.
   * Otherwise the current state is left untouched and returns false.
   */
  FOLLY_NODISCARD bool tryToTransitionState(State expected, State newState);

  /**
   * Transition from expected -> newState.
   *
   * Throws an error if the current state does not match the expected state.
   */
  void transitionState(State expected, State newState);

  /**
   * Transition from the STARTING state to the FUSE_ERROR state.
   *
   * Preconditions:
   * - `getState()` is STARTING or DESTROYING or SHUTTING_DOWN or SHUT_DOWN.
   *
   * Postconditions:
   * - If `getState()` was STARTING, `getState()` is now FUSE_ERROR.
   * - If `getState()` was not STARTING, `getState()` is unchanged.
   */
  void transitionToFuseInitializationErrorState();

  EdenMount(
      std::unique_ptr<CheckoutConfig> config,
      std::shared_ptr<ObjectStore> objectStore,
      std::shared_ptr<BlobCache> blobCache,
      std::shared_ptr<ServerState> serverState,
      std::unique_ptr<Journal> journal);

  // Forbidden copy constructor and assignment operator
  EdenMount(EdenMount const&) = delete;
  EdenMount& operator=(EdenMount const&) = delete;

  folly::Future<TreeInodePtr> createRootInode(
      const ParentCommits& parentCommits);

  FOLLY_NODISCARD folly::Future<folly::Unit> setupDotEden(TreeInodePtr root);

  folly::SemiFuture<SerializedInodeMap> shutdownImpl(bool doTakeover);

  /**
   * Create a DiffContext to be passed through the TreeInode diff codepath. This
   * will be used to record differences through the callback (in which
   * listIgnored determines if ignored files will be reported in the callback)
   * and houses the thrift request in order to check to see if the diff() should
   * be short circuited
   */
  std::unique_ptr<DiffContext> createDiffContext(
      DiffCallback* callback,
      bool listIgnored = false,
      apache::thrift::ResponseChannelRequest* FOLLY_NULLABLE request =
          nullptr) const;

  /**
   * This accepts a callback which will be invoked as differences are found.
   * Note that the callback methods may be invoked simultaneously from multiple
   * different threads, and the callback is responsible for performing
   * synchronization (if it is needed). It will be packaged into a DiffContext
   * and passed through the TreeInode diff() codepath
   */
  FOLLY_NODISCARD folly::Future<folly::Unit> diff(
      DiffCallback* callback,
      Hash commitHash,
      bool listIgnored,
      bool enforceCurrentParent,
      apache::thrift::ResponseChannelRequest* FOLLY_NULLABLE request) const;

  /**
   * Signal to unmount() that fuseMount() or takeoverFuse() has started.
   *
   * beginMount() returns a reference to
   * *mountingUnmountingState_->channelMountPromise. To signal that the
   * fuseMount() has completed, set the promise's value (or exception) without
   * mountingUnmountingState_'s lock held.
   *
   * If unmount() was called in the past, beginMount() throws
   * EdenMountCancelled.
   *
   * Preconditions:
   * - `beginMount()` has not been called before.
   */
  FOLLY_NODISCARD folly::Promise<folly::Unit>& beginMount();

#ifdef _WIN32
  using channelType = PrjfsChannel*;
  using ChannelStopData = PrjfsChannel::StopData;
#else
  using channelType = folly::File;
  using ChannelStopData = FuseChannel::StopData;
#endif

  using StopFuture = folly::SemiFuture<ChannelStopData>;

  /**
   * Open the platform specific device and mount it.
   */
  folly::Future<channelType> channelMount(bool readOnly);

  /**
   * Construct the channel_ member variable.
   */
  void createChannel(channelType fuseDevice);

  /**
   * Once the channel has been initialized, set up callbacks to clean up
   * correctly when it shuts down.
   */
  void channelInitSuccessful(EdenMount::StopFuture&& channelCompleteFuture);

  /**
   * Private destructor.
   *
   * This should not be invoked by callers directly.  Use the destroy() method
   * above (or the EdenMountDeleter if you plan to store the EdenMount in a
   * std::unique_ptr or std::shared_ptr).
   */
  ~EdenMount();

  friend class TreePrefetchLease;
  void treePrefetchFinished() noexcept;

  static constexpr int kMaxSymlinkChainDepth = 40; // max depth of symlink chain

  const std::unique_ptr<const CheckoutConfig> config_;

  /**
   * A promise associated with the future returned from
   * EdenMount::getChannelCompletionFuture() that completes when the
   * fuseChannel has no work remaining and can be torn down.
   * The future yields the underlying fuseDevice descriptor; it can
   * be passed on during graceful restart or simply closed if we're
   * unmounting and shutting down completely.  In the unmount scenario
   * the device should be closed prior to calling EdenMount::shutdown()
   * so that the subsequent privilegedFuseUnmount() call won't block
   * waiting on us for a response.
   */
  folly::Promise<TakeoverData::MountInfo> channelCompletionPromise_;

  /**
   * Eden server state shared across multiple mount points.
   */
  std::shared_ptr<ServerState> serverState_;

  std::unique_ptr<InodeMap> inodeMap_;

  std::unique_ptr<EdenDispatcher> dispatcher_;

  std::shared_ptr<ObjectStore> objectStore_;
  std::shared_ptr<BlobCache> blobCache_;
  BlobAccess blobAccess_;
  std::shared_ptr<Overlay> overlay_;

#ifndef _WIN32
  OverlayFileAccess overlayFileAccess_;
#endif // !_WIN32
  InodeNumber dotEdenInodeNumber_{};

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

  std::unique_ptr<Journal> journal_;

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

  struct MountingUnmountingState {
    bool channelMountStarted() const noexcept;
    bool channelUnmountStarted() const noexcept;

    /**
     * Whether or not the mount(2) syscall has been called (via fuseMount).
     *
     * Use this promise to wait for fuseMount to finish.
     *
     * * Empty optional: fuseMount/mount(2) has not been called yet.
     *   (startChannel/fuseMount can be called.)
     * * Unfulfilled: fuseMount is in progress.
     * * Fulfilled with Unit: fuseMount completed successfully (via
     *   startChannel), or we took over the FUSE device from another process
     *   (via takeoverFuse). (startChannel or takeoverFuse can still be in
     *   progress.)
     * * Fulfilled with error: fuseMount failed, or fuseMount was cancelled.
     *
     * The state of this variable might not reflect whether the file system is
     * mounted. For example, if this promise is fulfilled with Unit, then
     * umount(8) is called by another process, the file system will not be
     * mounted.
     */
    std::optional<folly::Promise<folly::Unit>> channelMountPromise;

    /**
     * Whether or not unmount has been called.
     *
     * * Empty optional: unmount has not been called yet. (unmount can be
     *   called.)
     * * Unfulfilled: unmount is in progress, either waiting for a concurrent
     *   fuseMount to complete or waiting for fuseUnmount to complete.
     * * Fulfilled with Unit: unmount was called. fuseUnmount completed
     *   successfully, or fuseMount was never called for this EdenMount.
     * * Fulfilled with error: unmount was called, but fuseUnmount failed.
     *
     * The state of this variable might not reflect whether the file system is
     * unmounted.
     */
    std::optional<folly::SharedPromise<folly::Unit>> channelUnmountPromise;
  };

  folly::Synchronized<MountingUnmountingState> mountingUnmountingState_;

  /**
   * The current state of the mount point.
   */
  std::atomic<State> state_{State::UNINITIALIZED};

  /**
   * uid and gid that we'll set as the owners in the stat information
   * returned via initStatData().
   */
  folly::Synchronized<Owner> owner_;

  /**
   * The number of tree prefetches in progress for this mount point.
   */
  std::atomic<uint64_t> numPrefetchesInProgress_{0};

#ifdef _WIN32
  /**
   * This is the channel between ProjectedFS and rest of Eden.
   */
  std::unique_ptr<PrjfsChannel> channel_;
#else
  /**
   * The associated fuse channel to the kernel.
   */
  std::unique_ptr<FuseChannel, FuseChannelDeleter> channel_;
#endif // !_WIN32

  /**
   * The clock.  This is also available as serverState_->getClock().
   * We still keep it as a separate member variable for now so that getClock()
   * can be inline without having to include ServerState.h in this file.
   */
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

class EdenMountCancelled : public std::runtime_error {
 public:
  explicit EdenMountCancelled();
};

} // namespace eden
} // namespace facebook
