/*
 *  Copyright (c) 2004-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/fuse/FuseChannel.h"

#include <folly/Random.h>
#include <folly/experimental/logging/xlog.h>
#include <folly/test/TestUtils.h>
#include <gtest/gtest.h>
#include <unordered_map>
#include "eden/fs/fuse/Dispatcher.h"
#include "eden/fs/fuse/EdenStats.h"
#include "eden/fs/fuse/RequestData.h"
#include "eden/fs/testharness/FakeFuse.h"
#include "eden/fs/testharness/TestDispatcher.h"

using namespace facebook::eden;
using namespace facebook::eden::fusell;
using namespace std::literals::chrono_literals;
using folly::ByteRange;
using folly::Future;
using folly::Promise;
using folly::Random;
using folly::Unit;
using std::make_unique;
using std::unique_ptr;

namespace {

fuse_entry_out genRandomLookupResponse(uint64_t nodeid) {
  fuse_entry_out response;
  response.nodeid = nodeid;
  response.generation = Random::rand64();
  response.entry_valid = Random::rand64();
  response.attr_valid = Random::rand64();
  response.entry_valid_nsec = Random::rand32();
  response.attr_valid_nsec = Random::rand32();
  response.attr.ino = nodeid;
  response.attr.size = Random::rand64();
  response.attr.blocks = Random::rand64();
  response.attr.atime = Random::rand64();
  response.attr.mtime = Random::rand64();
  response.attr.ctime = Random::rand64();
  response.attr.atimensec = Random::rand32();
  response.attr.mtimensec = Random::rand32();
  response.attr.ctimensec = Random::rand32();
  response.attr.mode = Random::rand32();
  response.attr.nlink = Random::rand32();
  response.attr.uid = Random::rand32();
  response.attr.gid = Random::rand32();
  response.attr.rdev = Random::rand32();
  response.attr.blksize = Random::rand32();
  response.attr.padding = Random::rand32();
  return response;
}

class FuseChannelTest : public ::testing::Test {
 protected:
  unique_ptr<FuseChannel, FuseChannelDeleter> createChannel(
      size_t numThreads = 2) {
    return unique_ptr<FuseChannel, FuseChannelDeleter>(
        new FuseChannel(fuse_.start(), mountPath_, numThreads, &dispatcher_));
  }

  FuseChannel::StopFuture performInit(FuseChannel* channel) {
    auto initFuture = channel->initialize();
    EXPECT_FALSE(initFuture.isReady());

    // Send the INIT packet
    auto reqID = fuse_.sendInitRequest();

    // Wait for the INIT response
    auto response = fuse_.recvResponse();
    EXPECT_EQ(reqID, response.header.unique);
    EXPECT_EQ(0, response.header.error);
    EXPECT_EQ(
        sizeof(fuse_out_header) + sizeof(fuse_init_out), response.header.len);
    EXPECT_EQ(sizeof(fuse_init_out), response.body.size());

    // The init future should be ready very shortly after we receive the INIT
    // response.  The FuseChannel initialization thread makes the future ready
    // shortly after sending the INIT response.
    return initFuture.get(100ms);
  }

  FakeFuse fuse_;
  ThreadLocalEdenStats stats_;
  TestDispatcher dispatcher_{&stats_};
  AbsolutePath mountPath_{"/fake/mount/path"};
};

} // namespace

TEST_F(FuseChannelTest, testDestroyNeverInitialized) {
  // Create a FuseChannel and then destroy it without ever calling initialize()
  auto channel = createChannel();
}

TEST_F(FuseChannelTest, testInitDestroy) {
  // Initialize the FuseChannel then immediately invoke its destructor
  // without explicitly requesting it to stop or receiving a close on the fUSE
  // device.
  auto channel = createChannel();
  performInit(channel.get());
}

TEST_F(FuseChannelTest, testDestroyWithPendingInit) {
  // Create a FuseChannel, call initialize(), and then destroy the FuseChannel
  // without ever having seen the INIT request from the kernel.
  auto channel = createChannel();
  auto initFuture = channel->initialize();
  EXPECT_FALSE(initFuture.isReady());
}

TEST_F(FuseChannelTest, testInitDestroyRace) {
  // Send an INIT request and immediately destroy the FuseChannelTest
  // without waiting for initialization to complete.
  auto channel = createChannel();
  auto initFuture = channel->initialize();
  fuse_.sendInitRequest();
  channel.reset();

  // Wait for the initialization future to complete.
  // It's fine if it fails if the channel was destroyed before initialization
  // completed, or its fine if it succeeded first too.
  initFuture.wait(100ms);
}

TEST_F(FuseChannelTest, testInitUnmount) {
  auto channel = createChannel();
  auto completeFuture = performInit(channel.get());

  // Close the FakeFuse so that FuseChannel will think the mount point has been
  // unmounted.
  fuse_.close();

  // Wait for the FuseChannel to signal that it has finished.
  auto stopReason = std::move(completeFuture).get(100ms);
  EXPECT_EQ(stopReason, FuseChannel::StopReason::UNMOUNTED);
}

TEST_F(FuseChannelTest, testInitUnmountRace) {
  auto channel = createChannel();
  auto completeFuture = performInit(channel.get());

  // Close the FakeFuse so that FuseChannel will think the mount point has been
  // unmounted.  We then immediately destroy the FuseChannel without waiting
  // for the session complete future, so that destruction and unmounting race.
  fuse_.close();
  channel.reset();

  // Wait for the session complete future now.
  auto stopReason = std::move(completeFuture).get(100ms);
  EXPECT_TRUE(
      stopReason == FuseChannel::StopReason::UNMOUNTED ||
      stopReason == FuseChannel::StopReason::DESTRUCTOR)
      << "unexpected FuseChannel stop reason: " << static_cast<int>(stopReason);
}

TEST_F(FuseChannelTest, testInitErrorClose) {
  // Close the FUSE device while the FuseChannel is waiting on the INIT request
  auto channel = createChannel();
  auto initFuture = channel->initialize();
  fuse_.close();

  EXPECT_THROW_RE(
      initFuture.get(100ms),
      std::runtime_error,
      "FUSE mount \"/fake/mount/path\" was unmounted before we "
      "received the INIT packet");
}

TEST_F(FuseChannelTest, testInitErrorWrongPacket) {
  // Send a packet other than FUSE_INIT while the FuseChannel is waiting on the
  // INIT request
  auto channel = createChannel();
  auto initFuture = channel->initialize();

  // Use a fuse_init_in body, but FUSE_LOOKUP as the opcode
  struct fuse_init_in initArg = {};
  fuse_.sendRequest(FUSE_LOOKUP, FUSE_ROOT_ID, initArg);

  EXPECT_THROW_RE(
      initFuture.get(100ms),
      std::runtime_error,
      "expected to receive FUSE_INIT for \"/fake/mount/path\" "
      "but got FUSE_LOOKUP");
}

TEST_F(FuseChannelTest, testInitErrorOldVersion) {
  auto channel = createChannel();
  auto initFuture = channel->initialize();

  // Send 2.7 as the FUSE version, which is too old
  struct fuse_init_in initArg = {};
  initArg.major = 2;
  initArg.minor = 7;
  initArg.max_readahead = 0;
  initArg.flags = 0;
  fuse_.sendRequest(FUSE_INIT, FUSE_ROOT_ID, initArg);

  EXPECT_THROW_RE(
      initFuture.get(100ms),
      std::runtime_error,
      "Unsupported FUSE kernel version 2.7 while initializing "
      "\"/fake/mount/path\"");
}

TEST_F(FuseChannelTest, testInitErrorShortPacket) {
  auto channel = createChannel();
  auto initFuture = channel->initialize();

  // Send a short message
  uint32_t body = 5;
  static_assert(
      sizeof(body) < sizeof(struct fuse_init_in),
      "we intend to send a body shorter than a fuse_init_in struct");
  fuse_.sendRequest(FUSE_INIT, FUSE_ROOT_ID, body);

  EXPECT_THROW_RE(
      initFuture.get(100ms),
      std::runtime_error,
      "received partial FUSE_INIT packet on mount \"/fake/mount/path\": "
      "size=44");
  static_assert(
      sizeof(fuse_in_header) + sizeof(uint32_t) == 44,
      "validate the size in our error message check");
}

TEST_F(FuseChannelTest, testDestroyWithPendingRequests) {
  auto channel = createChannel();
  auto completeFuture = performInit(channel.get());

  // Send several lookup requests
  //
  // Note: it is currently important that we wait for the dispatcher to receive
  // each request before sending the next one.  The FuseChannel receive code
  // expects to receive exactly 1 request per read() call on the FUSE device.
  // Since we are sending over a socket rather than a real FUSE device we
  // cannot guarantee that our writes will not be coalesced unless we confirm
  // that the FuseChannel has read each request before sending the next one.
  auto id1 = fuse_.sendLookup(FUSE_ROOT_ID, "foobar");
  auto req1 = dispatcher_.waitForLookup(id1);

  auto id2 = fuse_.sendLookup(FUSE_ROOT_ID, "some_file.txt");
  auto req2 = dispatcher_.waitForLookup(id2);

  auto id3 = fuse_.sendLookup(FUSE_ROOT_ID, "main.c");
  auto req3 = dispatcher_.waitForLookup(id3);

  // Destroy the channel object
  channel.reset();

  // The completion future still should not be ready, since the lookup
  // requests are still pending.
  EXPECT_FALSE(completeFuture.isReady());

  auto checkLookupResponse = [](const FakeFuse::Response& response,
                                uint64_t requestID,
                                fuse_entry_out expected) {
    EXPECT_EQ(requestID, response.header.unique);
    EXPECT_EQ(0, response.header.error);
    EXPECT_EQ(
        sizeof(fuse_out_header) + sizeof(fuse_entry_out), response.header.len);
    EXPECT_EQ(
        ByteRange(
            reinterpret_cast<const uint8_t*>(&expected), sizeof(expected)),
        ByteRange(response.body.data(), response.body.size()));
  };

  // Respond to the lookup requests
  auto response1 = genRandomLookupResponse(9);
  req1.promise.setValue(response1);
  auto received = fuse_.recvResponse();
  checkLookupResponse(received, id1, response1);

  // We don't have to respond in order; respond to request 3 before 2
  auto response3 = genRandomLookupResponse(19);
  req3.promise.setValue(response3);
  received = fuse_.recvResponse();
  checkLookupResponse(received, id3, response3);

  // The completion future still shouldn't be ready since there is still 1
  // request outstanding.
  EXPECT_FALSE(completeFuture.isReady());

  auto response2 = genRandomLookupResponse(12);
  req2.promise.setValue(response2);
  received = fuse_.recvResponse();
  checkLookupResponse(received, id2, response2);

  // TODO: FuseChannel unfortunately doesn't mark requests as finished when it
  // sends the response.  It doesn't clean up its outstanding request data
  // until the folly::future::detail::Core object associated with the request
  // is destroyed.
  //
  // Therefore completeFuture won't get invoked now until we destroy our
  // promises, even though they have already been fulfilled.
  //
  // It would probably be better to make FuseChannel clean up the outstanding
  // request data when it sends the reply.
  req1.promise = Promise<fuse_entry_out>::makeEmpty();
  req2.promise = Promise<fuse_entry_out>::makeEmpty();
  EXPECT_FALSE(completeFuture.isReady())
      << "remove this entire TODO block if/when we change FuseChannel "
      << "to remove outstanding request data when it sends the response";
  req3.promise = Promise<fuse_entry_out>::makeEmpty();

  // The completion future should be ready now that the last request
  // is done.
  EXPECT_TRUE(completeFuture.isReady());
  std::move(completeFuture).get(100ms);
}
