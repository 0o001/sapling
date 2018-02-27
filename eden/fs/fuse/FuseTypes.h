/*
 *  Copyright (c) 2017-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once
#include <folly/File.h>
#include <folly/Format.h>
#include <functional>
#include <iosfwd>
#include <utility>
#ifdef __linux__
#include "eden/third-party/fuse_kernel_linux.h"
#else
#error need a fuse kernel header to be included for your OS!
#endif
namespace facebook {
namespace eden {
namespace fusell {

/**
 * Represents ino_t behind a slightly safer API.  In general, it is a bug if
 * Eden produces inode numbers with the value 0, so this class makes it harder
 * to do that on accident.
 */
struct InodeNumber {
  /// Default-initializes the inode number to 0.
  constexpr InodeNumber() noexcept = default;

  /**
   * Initializes with a given nonzero number.  Will assert in debug builds if
   * initialized to zero.
   */
  constexpr explicit InodeNumber(uint64_t ino) noexcept : rawValue_{ino} {
    DCHECK_NE(0, rawValue_);
  }

  enum NoCheck { NO_CHECK };

  /**
   * Initializes the inode number without checking it so the constructor is
   * guaranteed to be constexpr.
   */
  constexpr explicit InodeNumber(NoCheck, uint64_t ino) noexcept
      : rawValue_{ino} {}

  /**
   * Thrift does not support unsigned numbers, so it's common to instantiate
   * InodeNumber from int64_t.
   */
  static InodeNumber fromThrift(int64_t ino) {
    return InodeNumber{static_cast<uint64_t>(ino)};
  }

  /**
   * Returns a nonzero inode number.  Asserts in debug builds if zero.
   *
   * Use this accessor when handing inode numbers to FUSE.
   */
  uint64_t get() const {
    DCHECK_NE(0, rawValue_);
    return rawValue_;
  }

  /**
   * Returns true if initialized with a nonzero inode number.
   */
  constexpr bool hasValue() const {
    return rawValue_ != 0;
  }

  /**
   * Returns true if underlying value is zero.
   */
  constexpr bool empty() const {
    return rawValue_ == 0;
  }

  /**
   * Returns the underlying value whether or not it's zero.  Use this accessor
   * when debugging or in tests.
   */
  constexpr uint64_t getRawValue() const {
    return rawValue_;
  }

 private:
  uint64_t rawValue_{0};
};

inline bool operator==(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() == rhs.getRawValue();
}

inline bool operator!=(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() != rhs.getRawValue();
}

inline bool operator<(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() < rhs.getRawValue();
}
inline bool operator<=(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() <= rhs.getRawValue();
}
inline bool operator>(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() > rhs.getRawValue();
}
inline bool operator>=(InodeNumber lhs, InodeNumber rhs) {
  return lhs.getRawValue() >= rhs.getRawValue();
}

std::ostream& operator<<(std::ostream& os, InodeNumber ino);

// Define toAppend() so folly::to<string>(ino) will work.
void toAppend(InodeNumber ino, std::string* result);

// Define toAppend() so folly::to<fbstring>(ino) will work.
void toAppend(InodeNumber ino, folly::fbstring* result);

// using InodeNumber = uint64_t;

using FuseOpcode = decltype(std::declval<fuse_in_header>().opcode);

/** Encapsulates the fuse device & connection information for a mount point.
 * This is the data that is required to be passed to a new process when
 * performing a graceful restart in order to re-establish the FuseChannel.
 */
struct FuseChannelData {
  folly::File fd;
  fuse_init_out connInfo;
};

} // namespace fusell

/// The inode number of the mount's root directory.
constexpr fusell::InodeNumber kRootNodeId =
    fusell::InodeNumber{fusell::InodeNumber::NO_CHECK, 1};

} // namespace eden
} // namespace facebook

namespace std {
template <>
struct hash<facebook::eden::fusell::InodeNumber> {
  size_t operator()(facebook::eden::fusell::InodeNumber n) const {
    // TODO: It may be worth using a different hash function.  The default
    // std::hash for integers is the identity function.  But since we allocate
    // inode numbers monotonically, this should be okay.
    return std::hash<uint64_t>{}(n.getRawValue());
  }
};
} // namespace std

namespace folly {
template <>
class FormatValue<facebook::eden::fusell::InodeNumber> {
 public:
  explicit FormatValue(facebook::eden::fusell::InodeNumber ino) : ino_(ino) {}

  template <class FormatCallback>
  void format(FormatArg& arg, FormatCallback& cb) const {
    FormatValue<uint64_t>{ino_.getRawValue()}.format(arg, cb);
  }

 private:
  const facebook::eden::fusell::InodeNumber ino_;
};
} // namespace folly
