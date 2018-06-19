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
#include <folly/Range.h>
#include <sys/types.h>
#include <memory>

namespace folly {
class EventBase;
class File;
template <typename T>
class Future;
class Unit;
}

namespace facebook {
namespace eden {

class UserInfo;

/**
 * A helper class for performing operations that require elevated privileges.
 *
 * This sends the requests to a separate helper process that has the privileges
 * required to peform these operations.
 */
class PrivHelper {
 public:
  virtual ~PrivHelper() {}

  /**
   * Attach the PrivHelper to an EventBase.
   *
   * This specifies the EventBase that the PrivHelper will use to drive I/O
   * operations.
   *
   * This method must be called before using the PrivHelper, and it must be
   * called from the EventBase thread.
   */
  virtual void attachEventBase(folly::EventBase* eventBase) = 0;

  /**
   * Detach the PrivHelper from its EventBase.
   *
   * This method may only be called from the current EventBase thread.
   *
   * No further I/O can be performed on this PrivHelper until it is re-attached
   * to another EventBase.  Any outstanding requests will not complete until the
   * PrivHelper is attached to another EventBase.
   */
  virtual void detachEventBase() = 0;

  /**
   * Ask the privileged helper process to perform a fuse mount.
   *
   * Returns a folly::File object with the file descriptor containing the fuse
   * connection.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::File> fuseMount(
      folly::StringPiece mountPath) = 0;

  /**
   * Ask the priveleged helper process to perform a fuse unmount.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::Unit> fuseUnmount(
      folly::StringPiece mountPath) = 0;

  /**
   * @param clientPath Absolute path (that should be under
   *     .eden/clients/<client-name>/bind-mounts/) where the "real" storage is.
   * @param mountPath Absolute path where the bind mount should be applied.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::Unit> bindMount(
      folly::StringPiece clientPath,
      folly::StringPiece mountPath) = 0;

  /**
   * Inform the privhelper process that we are shutting down as part of a
   * graceful restart, and a new edenfs daemon will take over our existing
   * mount points without unmounting them.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::Unit> fuseTakeoverShutdown(
      folly::StringPiece mountPath) = 0;

  /**
   * Inform the privhelper process that we have taken over an existing mount
   * point from another edenfs process.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::Unit> fuseTakeoverStartup(
      folly::StringPiece mountPath,
      const std::vector<std::string>& bindMounts) = 0;

  /**
   * Tell the privhelper server to write all future log messages to the
   * specified file descriptor.
   */
  FOLLY_NODISCARD virtual folly::Future<folly::Unit> setLogFile(
      folly::File logFile) = 0;

  /**
   * setLogFileBlocking() is a wrapper around setLogFile() that blocks until
   * the call has completed.
   *
   * This method may only be called if the PrivHelper is not currently attached
   * to an EventBase.  This is primarily intended as a convenience method to
   * allow calling setLogFile() before the main process's EventBase loop has
   * started.
   */
  void setLogFileBlocking(folly::File logFile);

  /*
   * Explicitly stop the privhelper process.
   *
   * The privhelper process will exit automatically when the main process exits
   * even if this method is not called.  However, this method can be used to
   * explictly stop the privhelper process, and check its exit code.
   *
   * Note that when the privhelper is stopped it will unmount any outstanding
   * mounts points.
   *
   * If the privhelper exited normally, the exit code is returned.
   * If the privhelper was terminated due to a signal, the signal number is
   * returned as a negative number.
   *
   * Throws an exception if the privhelper was not running, or if any other
   * error occurs.
   */
  virtual int stop() = 0;
};

class PrivHelperServer;

/**
 * Fork a separate privileged helper process, for performing mounts.
 *
 * This function should be very early on during program initialization, before
 * any other threads are forked.  After it is called UserInfo::dropPrivileges()
 * should be called to return the desired user privileges.
 */
std::unique_ptr<PrivHelper> startPrivHelper(const UserInfo& userInfo);

/**
 * Start a privhelper process using a custom PrivHelperServer class.
 *
 * This is really only intended for use in unit tests.
 */
std::unique_ptr<PrivHelper> startPrivHelper(
    PrivHelperServer* server,
    const UserInfo& userInfo);

/**
 * Create a PrivHelper client object using the specified connection rather than
 * forking a new privhelper server process.
 *
 * This is primarily intended for use in unit tests.
 */
std::unique_ptr<PrivHelper> createTestPrivHelper(folly::File&& conn);

} // namespace eden
} // namespace facebook
