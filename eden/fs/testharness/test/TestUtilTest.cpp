/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/testharness/TestUtil.h"

#include <gtest/gtest.h>
#include "eden/fs/model/Hash.h"
#include "eden/utils/test/TestChecks.h"

using namespace facebook::eden;

TEST(TestUtil, makeTestHash) {
  EXPECT_EQ(
      Hash{"0000000000000000000000000000000000000001"}, makeTestHash("1"));
  EXPECT_EQ(
      Hash{"0000000000000000000000000000000000000022"}, makeTestHash("22"));
  EXPECT_EQ(
      Hash{"0000000000000000000000000000000000000abc"}, makeTestHash("abc"));
  EXPECT_EQ(
      Hash{"123456789abcdef0fedcba9876543210faceb00c"},
      makeTestHash("123456789abcdef0fedcba9876543210faceb00c"));
  EXPECT_EQ(Hash{"0000000000000000000000000000000000000000"}, makeTestHash(""));
  EXPECT_THROW_RE(
      makeTestHash("123456789abcdef0fedcba9876543210faceb00c1"),
      std::invalid_argument,
      "too big");
  EXPECT_THROW_RE(
      makeTestHash("z"), std::exception, "could not be unhexlified");
  EXPECT_THROW_RE(
      // There's a "g" in the string below
      makeTestHash("123456789abcdefgfedcba9876543210faceb00c"),
      std::exception,
      "could not be unhexlified");
}
