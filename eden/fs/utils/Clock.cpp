/*
 *  Copyright (c) 2017-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "Clock.h"

#include <system_error>

namespace facebook {
namespace eden {

timespec UnixClock::getRealtime() {
  timespec rv;
  if (clock_gettime(CLOCK_REALTIME, &rv)) {
    throw std::system_error(
        errno, std::generic_category(), "clock_gettime failed");
  }
  return rv;
}

} // namespace eden
} // namespace facebook
