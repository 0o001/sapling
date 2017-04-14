/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "MountPoint.h"

#include "Channel.h"
#include "Dispatcher.h"

#include <sys/stat.h>

namespace facebook {
namespace eden {
namespace fusell {

MountPoint::MountPoint(AbsolutePathPiece path, Dispatcher* dispatcher)
    : path_(path), uid_(getuid()), gid_(getgid()), dispatcher_{dispatcher} {}

MountPoint::~MountPoint() {}

void MountPoint::start(bool debug) {
  std::function<void()> onStop;
  return start(debug, onStop);
}

void MountPoint::start(bool debug, const std::function<void()>& onStop) {
  std::unique_lock<std::mutex> lock(mutex_);
  if (status_ != Status::UNINIT) {
    throw std::runtime_error("mount point has already been started");
  }

  status_ = Status::STARTING;
  auto runner = [this, debug, onStop]() {
    try {
      this->run(debug);
    } catch (const std::exception& ex) {
      std::lock_guard<std::mutex> guard(mutex_);
      if (status_ == Status::STARTING) {
        LOG(ERROR) << "error starting FUSE mount: " << folly::exceptionStr(ex);
        startError_ = std::current_exception();
        status_ = Status::ERROR;
        statusCV_.notify_one();
        return;
      } else {
        // We potentially could call onStop() with a pointer to the exception,
        // or nullptr when stopping normally.
        LOG(ERROR) << "unhandled error occurred while running FUSE mount: "
                   << folly::exceptionStr(ex);
      }
    }
    if (onStop) {
      onStop();
    }
  };
  auto t = std::thread(runner);
  // Detach from the thread after starting it.
  // The onStop() function will be called to allow the caller to perform
  // any clean up desired.  However, since it runs from inside the thread
  // it can't join the thread yet.
  t.detach();

  // Wait until the mount is started successfully.
  while (status_ == Status::STARTING) {
    statusCV_.wait(lock);
  }
  if (status_ == Status::ERROR) {
    std::rethrow_exception(startError_);
  }
}

void MountPoint::mountStarted() {
  std::lock_guard<std::mutex> guard(mutex_);
  // Don't update status_ if it has already been put into an error
  // state or something.
  if (status_ == Status::STARTING) {
    status_ = Status::RUNNING;
    statusCV_.notify_one();
  }
}

void MountPoint::run(bool debug) {
  // This next line is responsible for indirectly calling mount().
  dispatcher_->setMountPoint(this);
  channel_ = std::make_unique<Channel>(this);
  channel_->runSession(dispatcher_, debug);
  channel_.reset();
  dispatcher_->unsetMountPoint();
}

struct stat MountPoint::initStatData() const {
  struct stat st;
  memset(&st, 0, sizeof(st));

  st.st_uid = uid_;
  st.st_gid = gid_;
  // We don't really use the block size for anything.
  // 4096 is fairly standard for many file systems.
  st.st_blksize = 4096;

  return st;
}
}
}
} // facebook::eden::fusell
