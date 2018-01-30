/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/takeover/TakeoverClient.h"

#include <folly/experimental/logging/xlog.h>
#include <folly/io/Cursor.h>
#include <folly/io/async/EventBase.h>
#include <thrift/lib/cpp2/protocol/Serializer.h>
#include "eden/fs/takeover/TakeoverData.h"
#include "eden/fs/takeover/gen-cpp2/takeover_types.h"
#include "eden/fs/utils/FutureUnixSocket.h"

using apache::thrift::CompactSerializer;
using folly::IOBuf;
using std::string;

namespace facebook {
namespace eden {

TakeoverData takeoverMounts(
    AbsolutePathPiece socketPath,
    const std::set<int32_t>& supportedVersions) {
  folly::EventBase evb;
  folly::Expected<UnixSocket::Message, folly::exception_wrapper>
      expectedMessage;

  auto connectTimeout = std::chrono::seconds(1);
  FutureUnixSocket socket;
  socket.connect(&evb, socketPath.stringPiece(), connectTimeout)
      .then([&socket, supportedVersions] {
        // Send our protocol version so that the server knows
        // whether we're capable of handshaking successfully

        TakeoverVersionQuery query;
        query.versions = supportedVersions;

        return socket.send(
            CompactSerializer::serialize<folly::IOBufQueue>(query).move());
      })
      .then([&socket] {
        // Wait for the takeover data response
        auto timeout = std::chrono::seconds(60);
        return socket.receive(timeout);
      })
      .then([&expectedMessage](UnixSocket::Message&& msg) {
        expectedMessage = std::move(msg);
      })
      .onError([&expectedMessage](folly::exception_wrapper&& ew) {
        expectedMessage = folly::makeUnexpected(std::move(ew));
      })
      .ensure([&evb] { evb.terminateLoopSoon(); });

  evb.loop();

  if (!expectedMessage) {
    XLOG(ERR) << "error receiving takeover data: " << expectedMessage.error();
    expectedMessage.error().throw_exception();
  }
  auto& message = expectedMessage.value();

  auto data = TakeoverData::deserialize(&message.data);
  // Add 2 here for the lock file and the thrift socket
  if (data.mountPoints.size() + 2 != message.files.size()) {
    throw std::runtime_error(folly::to<string>(
        "received ",
        data.mountPoints.size(),
        " mount paths, but ",
        message.files.size(),
        " FDs (including the lock file FD)"));
  }
  data.lockFile = std::move(message.files[0]);
  data.thriftSocket = std::move(message.files[1]);
  for (size_t n = 0; n < data.mountPoints.size(); ++n) {
    auto& mountInfo = data.mountPoints[n];
    mountInfo.fuseFD = std::move(message.files[n + 2]);
  }

  return data;
}
} // namespace eden
} // namespace facebook
