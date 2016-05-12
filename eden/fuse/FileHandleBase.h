/*
 *  Copyright (c) 2016, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#pragma once
#include "BufVec.h"
#include "Dispatcher.h"
#include "PollHandle.h"
#include <folly/futures/Future.h>

namespace facebook {
namespace eden {
namespace fusell {

class Dispatcher;

class FileHandleBase {
 public:
  virtual ~FileHandleBase();

  /**
   * Get file attributes
   */
  virtual folly::Future<Dispatcher::Attr> getattr() = 0;

  /**
   * Set file attributes
   *
   * In the 'attr' argument only members indicated by the 'to_set'
   * bitmask contain valid values.  Other members contain undefined
   * values.
   *
   * @param attr the attributes
   * @param to_set bit mask of attributes which should be set
   */
  virtual folly::Future<Dispatcher::Attr> setattr(const struct stat& attr,
                                                  int to_set) = 0;

  /* The result of an ioctl operation */
  struct Ioctl {
    int result;
    BufVec buf;
  };

  /**
   * Ioctl
   *
   * Only well-formed (restricted) ioctls are supported.  These are ioctls
   * that have the argument size encoded using _IOR, _IOW, _IOWR macros.
   *
   * @param arg is the argument passed in from userspace
   * @param inputData is a copy of the arg data from userspace
   * @param outputSize is the maximum size of the output data
   */
  virtual folly::Future<Ioctl> ioctl(int cmd,
                                     const void* arg,
                                     folly::ByteRange inputData,
                                     size_t outputSize);

  /**
   * Poll for IO readiness
   *
   * Introduced in version 2.8
   *
   * Note: If ph is non-NULL, the client should notify
   * when IO readiness events occur by calling
   * ph->notify().
   *
   * Regardless of the number of times poll with a non-NULL ph
   * is received, single notification is enough to clear all.
   * Notifying more times incurs overhead but doesn't harm
   * correctness.
   *
   * Return the poll(2) revents mask.
   */
  virtual folly::Future<unsigned> poll(std::unique_ptr<PollHandle> ph);
};
}
}
}
