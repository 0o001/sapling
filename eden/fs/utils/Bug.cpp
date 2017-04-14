/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "Bug.h"

#include <folly/Conv.h>
#include <folly/ExceptionWrapper.h>
#include <glog/logging.h>

namespace {
static std::atomic<int> edenBugDisabledCount{0};
}

namespace facebook {
namespace eden {
EdenBug::EdenBug(const char* file, int lineNumber)
    : file_(file), lineNumber_(lineNumber), message_("!!BUG!! ") {}

EdenBug::EdenBug(EdenBug&& other) noexcept
    : file_(other.file_),
      lineNumber_(other.lineNumber_),
      message_(std::move(other.message_)) {
  other.throwOnDestruction_ = false;
}

EdenBug::~EdenBug() noexcept(false) {
  // If toException() has not been called, throw an exception on destruction.
  //
  // Throwing in a destructor is normally poor form, in case we were triggered
  // by stack unwinding of another exception.  However our callers should
  // always use EdenBug objects as temporaries when they want the EDEN_BUG()
  // macro to throw directly.  Therefore we shouldn't have been triggered
  // during stack unwinding of another exception.
  //
  // Callers should only ever store EdenBug objects if they plan to call
  // toException() on them.
  if (throwOnDestruction_) {
    toException().throwException();
  }
}

folly::exception_wrapper EdenBug::toException() {
  logError();
  throwOnDestruction_ = false;
  return folly::exception_wrapper(std::runtime_error(message_));
}

void EdenBug::logError() {
  // TODO: We should log to scuba here in addition to logging locally.
  google::LogMessage(file_, lineNumber_, google::GLOG_ERROR).stream()
      << message_;

#ifndef NDEBUG
  // Crash in debug builds.
  // However, allow test code to disable crashing so that we can exercise
  // EDEN_BUG() code paths in tests.
  if (edenBugDisabledCount.load() == 0) {
    abort();
  }
#endif
}

EdenBugDisabler::EdenBugDisabler() {
  ++edenBugDisabledCount;
}

EdenBugDisabler::~EdenBugDisabler() {
  --edenBugDisabledCount;
}
}
}
