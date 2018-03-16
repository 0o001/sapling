/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <boost/filesystem.hpp>
#include <folly/Exception.h>
#include <folly/experimental/logging/Init.h>
#include <folly/experimental/logging/xlog.h>
#include <folly/init/Init.h>
#include <folly/io/async/ScopedEventBaseThread.h>
#include <signal.h>
#include <sysexits.h>
#include "eden/fs/fuse/Dispatcher.h"
#include "eden/fs/fuse/FuseChannel.h"
#include "eden/fs/fuse/privhelper/PrivHelper.h"
#include "eden/fs/fuse/privhelper/UserInfo.h"
#include "eden/fs/utils/PathFuncs.h"

using namespace facebook::eden;
using namespace facebook::eden::fusell;
using folly::exceptionStr;
using folly::makeFuture;
using std::string;

DEFINE_int32(numFuseThreads, 4, "The number of FUSE worker threads");
DEFINE_string(logging, "", "The logging configuration");

namespace folly {
const char* getBaseLoggingConfig() {
  return "eden=DBG2,eden.fs.fuse=DBG7";
}
} // namespace folly

namespace {
class TestDispatcher : public Dispatcher {
 public:
  TestDispatcher(ThreadLocalEdenStats* stats, const UserInfo& identity)
      : Dispatcher(stats), identity_(identity) {}

  folly::Future<Attr> getattr(fusell::InodeNumber ino) override {
    if (ino == kRootNodeId) {
      struct stat st = {};
      st.st_ino = ino.get();
      st.st_mode = S_IFDIR | 0755;
      st.st_nlink = 2;
      st.st_uid = identity_.getUid();
      st.st_gid = identity_.getGid();
      st.st_blksize = 512;
      st.st_blocks = 1;
      return folly::makeFuture(Attr(st, /* timeout */ 0));
    }
    folly::throwSystemErrorExplicit(ENOENT);
  }

  UserInfo identity_;
};

void ensureEmptyDirectory(AbsolutePathPiece path) {
  boost::filesystem::path boostPath(
      path.stringPiece().begin(), path.stringPiece().end());

  XLOG(INFO) << "boost path: " << boostPath.native();
  if (!boost::filesystem::create_directories(boostPath)) {
    // This directory already existed.  Make sure it is empty.
    if (!boost::filesystem::is_empty(boostPath)) {
      throw std::runtime_error(
          folly::to<string>(path, " does not refer to an empty directory"));
    }
  }
}
} // namespace

int main(int argc, char** argv) {
  // Make sure to run this before any flag values are read.
  folly::init(&argc, &argv);
  if (argc != 2) {
    fprintf(stderr, "usage: test_mount PATH\n");
    return EX_NOPERM;
  }

  auto sigresult = signal(SIGPIPE, SIG_IGN);
  if (sigresult == SIG_ERR) {
    folly::throwSystemError("error ignoring SIGPIPE");
  }

  // Determine the desired user and group ID.
  if (geteuid() != 0) {
    fprintf(stderr, "error: fuse_tester must be started as root\n");
    return EX_NOPERM;
  }
  folly::checkPosixError(chdir("/"), "failed to chdir(/)");

  // Fork the privhelper process, then drop privileges.
  auto identity = UserInfo::lookup();
  auto privHelper = startPrivHelper(identity);
  identity.dropPrivileges();

  folly::initLogging(FLAGS_logging);

  auto mountPath = normalizeBestEffort(argv[1]);
  try {
    ensureEmptyDirectory(mountPath);
  } catch (const std::exception& ex) {
    fprintf(stderr, "error with mount path: %s\n", exceptionStr(ex).c_str());
    return EX_DATAERR;
  }

  auto fuseDevice = privHelper->fuseMount(mountPath.value());
  ThreadLocalEdenStats stats;
  TestDispatcher dispatcher(&stats, identity);

  FuseChannel channel(
      std::move(fuseDevice), mountPath, FLAGS_numFuseThreads, &dispatcher);

  XLOG(INFO) << "Starting FUSE...";
  auto completionFuture = channel.initialize().get();
  XLOG(INFO) << "FUSE started";

  auto reason = std::move(completionFuture).get();
  XLOG(INFO) << "FUSE channel done; stop_reason=" << static_cast<int>(reason);

  return EX_OK;
}
