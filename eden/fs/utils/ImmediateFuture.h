/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/futures/Future.h>
#include "eden/fs/utils/ImmediateFuture-pre.h"

namespace facebook::eden {

/**
 * An ImmediateFuture is a Future type with similar semantics to folly::Future
 * except that it optimizes the already-fulfilled case by storing a Try<T>
 * inline. This allows code to not pay the allocation and atomic refcounting
 * overhead of folly::SemiFuture when an immediate value is available.
 *
 * Unlike Future and like SemiFuture, ImmediateFuture will never run an attached
 * callback on the thread that fulfills the corresponding Promise.
 *
 * Like Future and unlike SemiFuture, callbacks must handle running immediately.
 * An attached callback may run either immediately or later, when the
 * ImmediateFuture's value is consumed.
 *
 * All methods can throw an DestroyedImmediateFutureError if an ImmediateFuture
 * is used after being destroyed. This can happen if an ImmediateFuture is used
 * after being moved.
 */
template <typename T>
class ImmediateFuture {
 public:
  /**
   * The type of the stored value.
   */
  using value_type = T;

  /**
   * Default construct an ImmediateFuture with T's default constructor.
   */
  ImmediateFuture() noexcept(std::is_nothrow_default_constructible_v<T>)
      : immediate_{folly::Try<T>{T{}}}, kind_{Kind::Immediate} {}

  /**
   * Construct an ImmediateFuture with an already constructed value. No
   * folly::SemiFuture will be allocated.
   */
  /* implicit */ ImmediateFuture(folly::Try<T>&& value) noexcept(
      std::is_nothrow_move_constructible_v<folly::Try<T>>)
      : immediate_{std::move(value)}, kind_{Kind::Immediate} {}

  /**
   * Construct an ImmediateFuture with an already constructed value. No
   * folly::SemiFuture will be allocated.
   */
  /* implicit */ ImmediateFuture(T value) noexcept(
      std::is_nothrow_move_constructible_v<folly::Try<T>>)
      : ImmediateFuture{folly::Try<T>{std::move(value)}} {}

  /**
   * Construct an ImmediateFuture with a SemiFuture.
   *
   * If the given SemiFuture is ready, the resulting value is moved into and
   * stored inline in this ImmediateFuture.
   *
   * If lazy evaluation of SemiFuture's callbacks is intentional, an unfulfilled
   * SemiFuture can be created with `.defer()` and passed in.
   */
  /* implicit */ ImmediateFuture(folly::SemiFuture<T>&& fut) noexcept(
      std::is_nothrow_move_constructible_v<folly::SemiFuture<T>>);

  ~ImmediateFuture();

  ImmediateFuture(const ImmediateFuture<T>&) = delete;
  ImmediateFuture<T>& operator=(const ImmediateFuture<T>&) = delete;

  ImmediateFuture(ImmediateFuture<T>&&) noexcept;
  ImmediateFuture<T>& operator=(ImmediateFuture<T>&&) noexcept;

  /**
   * Queue the func continuation once this future is ready.
   *
   * If this ImmediateFuture already has a value, `func` will be called without
   * waiting. Otherwise, it will be called on the executor on which the end of
   * the Future chain is scheduled with `SemiFuture::via()`.
   *
   * Func must be a function taking a T as the only argument. Its return value
   * must be of a type that an ImmediateFuture can be constructed from
   * (folly::Try, folly::SemiFuture, etc).
   *
   * This method must be called with an rvalue-ref and should no longer be used
   * afterwards:
   *
   *   ImmediateFuture<int> fut{42};
   *   std::move(fut).thenValue([](int value) { return value + 1; });
   */
  template <typename Func>
  ImmediateFuture<detail::continuation_result_t<Func, T>> thenValue(
      Func&& func) &&;

  /**
   * Queue the func continuation once this future is ready.
   *
   * If this ImmediateFuture already has a value, `func` will be called without
   * waiting. Otherwise, it will be called on the executor on which the end of
   * the Future chain is scheduled with `SemiFuture::via()`.
   *
   * Func must be a function taking a Try<T> as the only argument, its return
   * value must be of a type that an ImmediateFuture can be constructed from
   * (folly::Try, folly::SemiFuture, etc).
   *
   * This method must be called with an rvalue-ref and should no longer be used
   * afterwards:
   *
   *   ImmediateFuture<int> fut{42};
   *   std::move(fut).thenTry([](Try<int> value) { return *value + 1; });
   */
  template <typename Func>
  ImmediateFuture<detail::continuation_result_t<Func, folly::Try<T>>> thenTry(
      Func&& func) &&;

  /**
   * Call func unconditionally once this future is ready and
   * the value/exception is passed through to the resulting Future.
   *
   * Func is like std::function<void()>. If func throws, its exception
   * will be propagated and the original value/exception discarded.
   *
   * This method must be called with an rvalue-ref and should no longer be used
   * afterwards:
   *
   *   ImmediateFuture<int> fut{42};
   *   std::move(fut).ensure([&]() { cleanup(); });
   */
  template <typename Func>
  ImmediateFuture<T> ensure(Func&& func) &&;

  /**
   * Returns true if a value is immediately available.
   *
   * That is, if isReady() returns true, calling `thenValue` or `thenTry` is
   * guaranteed to run the callback immediately.
   */
  bool isReady() const;

  /**
   * Build a SemiFuture out of this ImmediateFuture and returns it.
   *
   * The returned semi future can then be executed on an executor with its
   * via() method. When this ImmediateFuture stores an immediate value, this
   * will allocate a new SemiFuture that is ready.
   *
   * Can be used as such:
   *
   *   ImmediateFuture<T> immFut = ...;
   *   folly::Future<T> fut = std::move(fut).semi().via(executor);
   */
  folly::SemiFuture<T> semi() &&;

  /**
   * Wait for the future to complete and return its value or throw its
   * exception.
   *
   * When the future is an immediate value, this returns without waiting.
   */
  T get() &&;

  /**
   * Wait for the future to complete and return the Try value.
   *
   * When the future is an immediate value, this returns without waiting.
   */
  folly::Try<T> getTry() &&;

  /**
   * Wait for the future to complete and return its value or throw its
   * exception.
   *
   * When the future is an immediate value, this returns without waiting.
   *
   * A folly::FutureTimeout will be thrown if the timeout is reached.
   */
  T get(folly::HighResDuration timeout) &&;

  /**
   * Wait for the future to complete and return the Try value.
   *
   * When the future is an immediate value, this returns without waiting.
   *
   * A folly::FutureTimeout will be thrown if the timeout is reached.
   */
  folly::Try<T> getTry(folly::HighResDuration timeout) &&;

 private:
  /**
   * Destroy this ImmediatureFuture.
   *
   * Any subsequent access to it will throw a DestroyedImmediateFutureError.
   */
  void destroy();

  union {
    folly::Try<T> immediate_;
    folly::SemiFuture<T> semi_;
  };

  enum class Kind {
    /** Holds an immediate value, immediate_ is valid. */
    Immediate,
    /** Holds a SemiFuture, semi_ is valid. */
    SemiFuture,
    /** Doesn't hold anything, neither immediate_ nor semi_ are valid. */
    Nothing,
  };

  // TODO: At the cost of reimplementing parts of Try, we could save a byte or
  // four by merging these tag bits with Try's tag bits, and differentiate
  // between Value, Exception, SemiFuture, and Nothing.
  Kind kind_;
};

/**
 * Exception thrown if the ImmediateFuture is used after being destroyed.
 */
class DestroyedImmediateFutureError : public std::logic_error {
 public:
  DestroyedImmediateFutureError()
      : std::logic_error{"ImmediateFuture used after destruction"} {}
};

/**
 * Build an ImmediateFuture from func.
 *
 * Exceptions thrown by func will be captured in the returned ImmediateFuture.
 *
 * This is a shorthand for:
 *
 *   ImmediateFuture<folly::Unit>().thenTry([](auto&&) { return func(); });
 */
template <typename Func>
auto makeImmediateFutureWith(Func&& func);

/**
 * Run all the passed in ImmediateFuture to completion.
 *
 * The returned ImmediateFuture will complete when all the passed in
 * ImmediateFuture have completed. The returned vector keeps the same ordering
 * as the given vector ImmediateFuture.
 */
template <typename T>
ImmediateFuture<std::vector<folly::Try<T>>> collectAll(
    std::vector<ImmediateFuture<T>> futures);

} // namespace facebook::eden

#include "eden/fs/utils/ImmediateFuture-inl.h"
