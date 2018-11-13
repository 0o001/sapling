/*
 *  Copyright (c) 2018-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/benchharness/Bench.h"
#include <inttypes.h>
#include <stdio.h>
#include <time.h>

namespace facebook {
namespace eden {

StartingGate::StartingGate(size_t threadCount) : totalThreads_{threadCount} {}

void StartingGate::wait() {
  std::unique_lock lock{mutex_};
  ++waitingThreads_;
  cv_.notify_all();
  cv_.wait(lock, [&] { return ready_; });
}

void StartingGate::waitForWaitingThreads() {
  std::unique_lock lock{mutex_};
  cv_.wait(lock, [&] { return waitingThreads_ == totalThreads_; });
}

void StartingGate::open() {
  std::unique_lock lock{mutex_};
  ready_ = true;
  cv_.notify_all();
}

uint64_t getTime() noexcept {
  timespec ts;
  // CLOCK_MONOTONIC is subject in NTP adjustments. CLOCK_MONOTONIC_RAW would be
  // better but these benchmarks are short and reading CLOCK_MONOTONIC takes 20
  // ns and CLOCK_MONOTONIC_RAW takes 130 ns.
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return ts.tv_sec * 1000000000 + ts.tv_nsec;
}

StatAccumulator measureClockOverhead() noexcept {
  constexpr int N = 10000;

  StatAccumulator accum;

  uint64_t last = getTime();
  for (int i = 0; i < N; ++i) {
    uint64_t next = getTime();
    uint64_t elapsed = next - last;
    accum.add(elapsed);
    last = next;
  }

  return accum;
}

} // namespace eden
} // namespace facebook
