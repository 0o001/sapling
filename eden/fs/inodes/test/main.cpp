/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include <folly/experimental/logging/Init.h>
#include <folly/init/Init.h>
#include <gtest/gtest.h>

DEFINE_string(logging, "", "folly::logging configuration");

int main(int argc, char* argv[]) {
  testing::InitGoogleTest(&argc, argv);
  folly::init(&argc, &argv);
  folly::initLogging(FLAGS_logging);

  // The FuseChannel code sends SIGPIPE and expects it to be ignored.
  ::signal(SIGPIPE, SIG_IGN);

  return RUN_ALL_TESTS();
}
