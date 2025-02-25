/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/hg/HgImportRequestQueue.h"
#include <folly/MapUtil.h>
#include <folly/futures/Future.h>
#include <algorithm>
#include "eden/fs/config/ReloadableConfig.h"

namespace facebook::eden {

void HgImportRequestQueue::stop() {
  auto state = state_.lock();
  if (state->running) {
    state->running = false;
    queueCV_.notify_all();
  }
}

folly::Future<BlobPtr> HgImportRequestQueue::enqueueBlob(
    std::shared_ptr<HgImportRequest> request) {
  return enqueue<Blob, HgImportRequest::BlobImport>(std::move(request));
}

folly::Future<TreePtr> HgImportRequestQueue::enqueueTree(
    std::shared_ptr<HgImportRequest> request) {
  return enqueue<Tree, HgImportRequest::TreeImport>(std::move(request));
}

folly::Future<BlobMetadataPtr> HgImportRequestQueue::enqueueBlobMeta(
    std::shared_ptr<HgImportRequest> request) {
  return enqueue<BlobMetadata, HgImportRequest::BlobMetaImport>(
      std::move(request));
}

template <typename T, typename ImportType>
folly::Future<std::shared_ptr<const T>> HgImportRequestQueue::enqueue(
    std::shared_ptr<HgImportRequest> request) {
  auto state = state_.lock();
  auto* importQueue = getImportQueue<const T>(state);
  auto* requestQueue = &importQueue->queue;

  const auto& hash = request->getRequest<ImportType>()->hash;
  if (auto* existingRequestPtr =
          folly::get_ptr(importQueue->requestTracker, hash)) {
    auto& existingRequest = *existingRequestPtr;
    auto* trackedImport = existingRequest->template getRequest<ImportType>();

    auto [promise, future] =
        folly::makePromiseContract<std::shared_ptr<const T>>();
    trackedImport->promises.emplace_back(std::move(promise));

    if (existingRequest->getPriority() < request->getPriority()) {
      existingRequest->setPriority(request->getPriority());

      // Since the new request has a higher priority than the already present
      // one, we need to re-order the heap.
      //
      // TODO(xavierd): this has a O(n) complexity, and enqueing tons of
      // duplicated requests will thus lead to a quadratic complexity.
      std::make_heap(
          requestQueue->begin(),
          requestQueue->end(),
          [](const std::shared_ptr<HgImportRequest>& lhs,
             const std::shared_ptr<HgImportRequest>& rhs) {
            return (*lhs) < (*rhs);
          });
    }

    return std::move(future).toUnsafeFuture();
  }

  requestQueue->emplace_back(request);
  auto promise = request->getPromise<std::shared_ptr<const T>>();

  importQueue->requestTracker.emplace(hash, std::move(request));

  std::push_heap(
      requestQueue->begin(),
      requestQueue->end(),
      [](const std::shared_ptr<HgImportRequest>& lhs,
         const std::shared_ptr<HgImportRequest>& rhs) {
        return (*lhs) < (*rhs);
      });

  queueCV_.notify_one();

  return promise->getFuture();
}

std::vector<std::shared_ptr<HgImportRequest>>
HgImportRequestQueue::combineAndClearRequestQueues() {
  auto state = state_.lock();
  auto treeQSz = state->treeQueue.queue.size();
  auto blobQSz = state->blobQueue.queue.size();
  auto blobMetaQSz = state->blobMetaQueue.queue.size();
  XLOGF(
      DBG5,
      "combineAndClearRequestQueues: tree queue size = {}, blob queue size = {}, blob metadata queue size = {}",
      treeQSz,
      blobQSz,
      blobMetaQSz);
  auto res = std::move(state->treeQueue.queue);
  res.insert(
      res.end(),
      std::make_move_iterator(state->blobQueue.queue.begin()),
      std::make_move_iterator(state->blobQueue.queue.end()));
  res.insert(
      res.end(),
      std::make_move_iterator(state->blobMetaQueue.queue.begin()),
      std::make_move_iterator(state->blobMetaQueue.queue.end()));
  state->treeQueue.queue.clear();
  state->blobQueue.queue.clear();
  state->blobMetaQueue.queue.clear();
  XCHECK_EQ(res.size(), treeQSz + blobQSz + blobMetaQSz);
  return res;
}

std::vector<std::shared_ptr<HgImportRequest>> HgImportRequestQueue::dequeue() {
  size_t count;
  std::vector<std::shared_ptr<HgImportRequest>>* queue = nullptr;

  auto state = state_.lock();
  while (true) {
    if (!state->running) {
      state->treeQueue.queue.clear();
      state->blobQueue.queue.clear();
      state->blobMetaQueue.queue.clear();
      return std::vector<std::shared_ptr<HgImportRequest>>();
    }

    auto highestPriority = ImportPriority::minimumValue();

    // Trees have a higher priority than blobs, thus check the queues in that
    // order.  The reason for trees having a higher priority is due to trees
    // allowing a higher fan-out and thus increasing concurrency of fetches
    // which translate onto a higher overall throughput.
    if (!state->treeQueue.queue.empty()) {
      count = config_->getEdenConfig()->importBatchSizeTree.getValue();
      highestPriority = state->treeQueue.queue.front()->getPriority();
      queue = &state->treeQueue.queue;
    }

    if (!state->blobMetaQueue.queue.empty()) {
      auto priority = state->blobMetaQueue.queue.front()->getPriority();
      if (!queue || priority > highestPriority) {
        queue = &state->blobMetaQueue.queue;
        count = config_->getEdenConfig()->importBatchSizeBlobMeta.getValue();
        highestPriority = priority;
      }
    }

    if (!state->blobQueue.queue.empty()) {
      auto priority = state->blobQueue.queue.front()->getPriority();
      if (!queue || priority > highestPriority) {
        queue = &state->blobQueue.queue;
        count = config_->getEdenConfig()->importBatchSize.getValue();
        highestPriority = priority;
      }
    }

    if (queue) {
      break;
    } else {
      queueCV_.wait(state.as_lock());
    }
  }

  count = std::min(count, queue->size());
  std::vector<std::shared_ptr<HgImportRequest>> result;
  result.reserve(count);
  for (size_t i = 0; i < count; i++) {
    std::pop_heap(
        queue->begin(),
        queue->end(),
        [](const std::shared_ptr<HgImportRequest>& lhs,
           const std::shared_ptr<HgImportRequest>& rhs) {
          return (*lhs) < (*rhs);
        });

    result.emplace_back(std::move(queue->back()));
    queue->pop_back();
  }

  return result;
}

} // namespace facebook::eden
