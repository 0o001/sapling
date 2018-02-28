/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/testharness/FakeFuse.h"

#include <folly/Exception.h>
#include <folly/FileUtil.h>
#include <folly/chrono/Conv.h>
#include <folly/experimental/logging/xlog.h>
#include <sys/socket.h>
#include <sys/types.h>
#include "eden/third-party/fuse_kernel_linux.h"

using namespace std::literals::chrono_literals;
using folly::ByteRange;
using std::string;

namespace facebook {
namespace eden {

FakeFuse::FakeFuse() {}

folly::File FakeFuse::start() {
  std::array<int, 2> sockets;
  folly::checkUnixError(
      socketpair(AF_UNIX, SOCK_STREAM, 0, sockets.data()),
      "socketpair() failed");
  conn_ = folly::File(sockets[0], /* ownsFd */ true);
  auto userConn = folly::File(sockets[1], /* ownsFd */ true);

  // Set a timeout so the tests will fail quickly if we don't have
  // data ready when we expect to.
  setTimeout(1s);

  return userConn;
}

void FakeFuse::close() {
  conn_.close();
}

bool FakeFuse::isStarted() const {
  return conn_.fd() != -1;
}

void FakeFuse::setTimeout(std::chrono::milliseconds timeout) {
  auto tv = folly::to<struct timeval>(timeout);
  // recvResponse() and sendRequest() both perform blocking I/O.
  // We simply set to the socket timeout to force the blocking recv/send calls
  // to time out if they do not complete within the specified timeout.
  folly::checkUnixError(
      setsockopt(conn_.fd(), SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv)));
  folly::checkUnixError(
      setsockopt(conn_.fd(), SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv)));
}

uint32_t FakeFuse::sendRequest(uint32_t opcode, uint64_t inode, ByteRange arg) {
  auto requestID = requestID_;
  ++requestID_;
  XLOG(DBG5) << "injecting FUSE request ID " << requestID
             << ": opcode= " << opcode;

  fuse_in_header header = {};
  header.len = sizeof(struct fuse_in_header) + arg.size();
  header.opcode = opcode;
  header.unique = requestID;
  header.nodeid = inode;

  std::array<iovec, 2> iov;
  iov[0].iov_base = &header;
  iov[0].iov_len = sizeof(struct fuse_in_header);
  iov[1].iov_base = const_cast<uint8_t*>(arg.data());
  iov[1].iov_len = arg.size();

  folly::checkUnixError(
      folly::writevFull(conn_.fd(), iov.data(), iov.size()),
      "failed to send FUSE request ");
  return requestID;
}

void FakeFuse::recvFull(void* buf, size_t len) {
  char* ptr = static_cast<char*>(buf);
  auto bytesLeft = len;
  while (bytesLeft > 0) {
    auto bytesRead = recv(conn_.fd(), ptr, bytesLeft, 0);
    if (bytesRead < 0) {
      if (errno == EINTR) {
        continue;
      }
      folly::throwSystemError("error receiving data on fake FUSE connection");
    }

    ptr += bytesRead;
    bytesLeft -= bytesRead;
  }
}

FakeFuse::Response FakeFuse::recvResponse() {
  Response response;

  recvFull(&response.header, sizeof(response.header));
  if (response.header.len < sizeof(fuse_out_header)) {
    throw std::runtime_error(folly::to<string>(
        "received FUSE response with invalid length: ",
        response.header.len,
        " is shorter than the response header size"));
  }
  auto bodySize = response.header.len - sizeof(fuse_out_header);

  response.body.resize(bodySize);
  recvFull(response.body.data(), bodySize);

  return response;
}

} // namespace eden
} // namespace facebook
