/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include <folly/init/Init.h>
#include <folly/portability/GTest.h>

int main(int argc, char* argv[]) {
  testing::InitGoogleTest(&argc, argv);
  folly::init(&argc, &argv);

#ifndef _WIN32
  // The FuseChannel code sends SIGPIPE and expects it to be ignored.
  ::signal(SIGPIPE, SIG_IGN);
#endif

  return RUN_ALL_TESTS();
}
