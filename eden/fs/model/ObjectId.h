/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <boost/operators.hpp>
#include <fmt/format.h>
#include <folly/FBString.h>
#include <folly/Range.h>
#include <stdint.h>
#include <array>
#include <iosfwd>

namespace folly {
class IOBuf;
}

namespace facebook::eden {

/**
   Identifier of objects in local store.
   This identifier is a variable length string.
*/
class ObjectId : boost::totally_ordered<ObjectId> {
 public:
  // fbstring has more SSO space (23 bytes!) than std::string and thus can hold
  // 20-byte hashes inline.
  using Storage = folly::fbstring;

  /**
   * Create an empty object id
   */
  ObjectId() noexcept : bytes_{} {}

  explicit ObjectId(folly::ByteRange bytes)
      : bytes_{constructFromByteRange(bytes)} {}

  /**
   * @param hex is a string of 40 hexadecimal characters.
   */
  explicit ObjectId(folly::StringPiece hex) : bytes_{constructFromHex(hex)} {}

  /**
   * Compute the SHA1 hash of an IOBuf chain.
   */
  static ObjectId sha1(const folly::IOBuf& buf);

  /**
   * Compute the SHA1 hash of a std::string.
   */
  static ObjectId sha1(const std::string& str) {
    return sha1(folly::ByteRange{folly::StringPiece{str}});
  }

  /**
   * Compute the SHA1 hash of a ByteRange.
   */
  static ObjectId sha1(folly::ByteRange data);

  /**
   * Returns bytes content of the ObjectId
   */
  folly::ByteRange getBytes() const {
    return folly::ByteRange{folly::StringPiece{bytes_}};
  }

  /** @return [lowercase] hex representation of this ObjectId. */
  std::string toLogString() const {
    return asHexString();
  }

  std::string asHexString() const;

  /** @return bytes of this ObjectId. */
  std::string asString() const;

  size_t getHashCode() const noexcept;

  bool operator==(const ObjectId&) const;
  bool operator<(const ObjectId&) const;

 private:
  static Storage constructFromByteRange(folly::ByteRange bytes) {
    return Storage{(const char*)bytes.data(), bytes.size()};
  }
  static Storage constructFromHex(folly::StringPiece hex) {
    if (hex.size() % 2 != 0) {
      throwInvalidArgument(
          "incorrect data size for Hash constructor from string: ", hex.size());
    }
    folly::fbstring result;
    auto size = hex.size() / 2;
    result.reserve(size);
    for (size_t i = 0; i < size; i++) {
      result.push_back(hexByteAt(hex, i));
    }
    return result;
  }
  static constexpr char hexByteAt(folly::StringPiece hex, size_t index) {
    return (nibbleToHex(hex.data()[index * 2]) * 16) +
        nibbleToHex(hex.data()[(index * 2) + 1]);
  }
  static constexpr char nibbleToHex(char c) {
    if ('0' <= c && c <= '9') {
      return c - '0';
    } else if ('a' <= c && c <= 'f') {
      return 10 + c - 'a';
    } else if ('A' <= c && c <= 'F') {
      return 10 + c - 'A';
    } else {
      throwInvalidArgument(
          "invalid hex digit supplied to Hash constructor from string: ", c);
    }
  }

  [[noreturn]] static void throwInvalidArgument(
      const char* message,
      size_t number);

  Storage bytes_;
};

using ObjectIdRange = folly::Range<const ObjectId*>;

/**
 * Output stream operator for ObjectId.
 *
 * This makes it possible to easily use ObjectId in glog statements.
 */
std::ostream& operator<<(std::ostream& os, const ObjectId& hash);

/* Define toAppend() so folly::to<string>(Hash) will work */
void toAppend(const ObjectId& hash, std::string* result);

} // namespace facebook::eden

namespace std {
template <>
struct hash<facebook::eden::ObjectId> {
  size_t operator()(const facebook::eden::ObjectId& hash) const noexcept {
    return hash.getHashCode();
  }
};
} // namespace std

namespace fmt {
template <>
struct formatter<facebook::eden::ObjectId> : formatter<std::string> {
  auto format(const facebook::eden::ObjectId& id, format_context& ctx) {
    return formatter<std::string>::format(id.toLogString(), ctx);
  }
};
} // namespace fmt
