/*
 *  Copyright (c) 2016-present, Facebook, Inc.
 *  All rights reserved.
 *
 *  This source code is licensed under the BSD-style license found in the
 *  LICENSE file in the root directory of this source tree. An additional grant
 *  of patent rights can be found in the PATENTS file in the same directory.
 *
 */
#include "eden/fs/store/RocksDbLocalStore.h"
#include <folly/Format.h>
#include <folly/String.h>
#include <folly/futures/Future.h>
#include <folly/io/Cursor.h>
#include <folly/io/IOBuf.h>
#include <folly/lang/Bits.h>
#include <folly/logging/xlog.h>
#include <rocksdb/db.h>
#include <rocksdb/filter_policy.h>
#include <rocksdb/table.h>
#include <array>
#include "eden/fs/rocksdb/RocksException.h"
#include "eden/fs/rocksdb/RocksHandles.h"
#include "eden/fs/store/StoreResult.h"
#include "eden/fs/utils/FaultInjector.h"

using facebook::eden::Hash;
using folly::ByteRange;
using folly::IOBuf;
using folly::StringPiece;
using folly::io::Cursor;
using rocksdb::FlushOptions;
using rocksdb::ReadOptions;
using rocksdb::Slice;
using rocksdb::SliceParts;
using rocksdb::WriteBatch;
using rocksdb::WriteOptions;
using std::string;
using std::unique_ptr;

namespace {
using namespace facebook::eden;

rocksdb::ColumnFamilyOptions makeColumnOptions(uint64_t LRUblockCacheSizeMB) {
  rocksdb::ColumnFamilyOptions options;

  // We'll never perform range scans on any of the keys that we store.
  // This enables bloom filters and a hash policy that improves our
  // get/put performance.
  options.OptimizeForPointLookup(LRUblockCacheSizeMB);

  options.OptimizeLevelStyleCompaction();
  return options;
}

/**
 * The different key spaces that we desire.
 * The ordering is coupled with the values of the LocalStore::KeySpace enum.
 */
const std::vector<rocksdb::ColumnFamilyDescriptor>& columnFamilies() {
  // Most of the column families will share the same cache.  We
  // want the blob data to live in its own smaller cache; the assumption
  // is that the vfs cache will compensate for that, together with the
  // idea that we shouldn't need to materialize a great many files.
  auto options = makeColumnOptions(64);
  auto blobOptions = makeColumnOptions(8);

  // Meyers singleton to avoid SIOF issues
  static const std::vector<rocksdb::ColumnFamilyDescriptor> families{
      rocksdb::ColumnFamilyDescriptor{rocksdb::kDefaultColumnFamilyName,
                                      options},
      rocksdb::ColumnFamilyDescriptor{"blob", blobOptions},
      rocksdb::ColumnFamilyDescriptor{"blobmeta", options},
      rocksdb::ColumnFamilyDescriptor{"tree", options},
      rocksdb::ColumnFamilyDescriptor{"hgproxyhash", options},
      rocksdb::ColumnFamilyDescriptor{"hgcommit2tree", options},
  };
  return families;
}

rocksdb::Slice _createSlice(folly::ByteRange bytes) {
  return Slice(reinterpret_cast<const char*>(bytes.data()), bytes.size());
}

class RocksDbWriteBatch : public LocalStore::WriteBatch {
 public:
  void put(
      LocalStore::KeySpace keySpace,
      folly::ByteRange key,
      folly::ByteRange value) override;
  void put(
      LocalStore::KeySpace keySpace,
      folly::ByteRange key,
      std::vector<folly::ByteRange> valueSlices) override;
  void flush() override;
  ~RocksDbWriteBatch() override;
  // Use LocalStore::beginWrite() to create a write batch
  RocksDbWriteBatch(RocksHandles& dbHandles, size_t bufferSize);

  void flushIfNeeded();

  RocksHandles& dbHandles_;
  rocksdb::WriteBatch writeBatch_;
  size_t bufSize_;
};

void RocksDbWriteBatch::flush() {
  auto pending = writeBatch_.Count();
  if (pending == 0) {
    return;
  }

  XLOG(DBG5) << "Flushing " << pending << " entries with data size of "
             << writeBatch_.GetDataSize();

  auto status = dbHandles_.db->Write(WriteOptions(), &writeBatch_);
  XLOG(DBG5) << "... Flushed";

  if (!status.ok()) {
    throw RocksException::build(
        status, "error putting blob batch in local store");
  }

  writeBatch_.Clear();
}

void RocksDbWriteBatch::flushIfNeeded() {
  auto needFlush = bufSize_ > 0 && writeBatch_.GetDataSize() >= bufSize_;

  if (needFlush) {
    flush();
  }
}

RocksDbWriteBatch::RocksDbWriteBatch(RocksHandles& dbHandles, size_t bufSize)
    : LocalStore::WriteBatch(),
      dbHandles_(dbHandles),
      writeBatch_(bufSize),
      bufSize_(bufSize) {}

RocksDbWriteBatch::~RocksDbWriteBatch() {
  if (writeBatch_.Count() > 0) {
    XLOG(ERR) << "WriteBatch being destroyed with " << writeBatch_.Count()
              << " items pending flush";
  }
}

void RocksDbWriteBatch::put(
    LocalStore::KeySpace keySpace,
    folly::ByteRange key,
    folly::ByteRange value) {
  writeBatch_.Put(
      dbHandles_.columns[keySpace].get(),
      _createSlice(key),
      _createSlice(value));

  flushIfNeeded();
}

void RocksDbWriteBatch::put(
    LocalStore::KeySpace keySpace,
    folly::ByteRange key,
    std::vector<folly::ByteRange> valueSlices) {
  std::vector<Slice> slices;

  for (auto& valueSlice : valueSlices) {
    slices.emplace_back(_createSlice(valueSlice));
  }

  auto keySlice = _createSlice(key);
  SliceParts keyParts(&keySlice, 1);
  writeBatch_.Put(
      dbHandles_.columns[keySpace].get(),
      keyParts,
      SliceParts(slices.data(), slices.size()));

  flushIfNeeded();
}

} // namespace

namespace facebook {
namespace eden {

RocksDbLocalStore::RocksDbLocalStore(
    AbsolutePathPiece pathToRocksDb,
    FaultInjector* faultInjector,
    std::shared_ptr<ReloadableConfig> config)
    : LocalStore(std::move(config)),
      faultInjector_(*faultInjector),
      dbHandles_(pathToRocksDb.stringPiece(), columnFamilies()),
      ioPool_(12, "RocksLocalStore") {
  writeColumnFamilyIDs();
}

RocksDbLocalStore::~RocksDbLocalStore() {
#ifdef FOLLY_SANITIZE_ADDRESS
  // RocksDB has some race conditions around setting up and tearing down
  // the threads that it uses to maintain the database.  This manifests
  // in our test harness, particularly in a test where we quickly mount
  // and then unmount.  We see this as an abort with the message:
  // "pthread lock: Invalid Argument".
  // My assumption is that we're shutting things down before rocks has
  // completed initializing.  This sleep call is present in the destructor
  // to make it more likely that rocks is past that critical point and
  // so that we can shutdown successfully.
  /* sleep override */ sleep(1);
#endif
}

void RocksDbLocalStore::close() {
  dbHandles_.columns.clear();
  dbHandles_.db.reset();
}

void RocksDbLocalStore::clearKeySpace(KeySpace keySpace) {
  auto columnFamily = dbHandles_.columns[keySpace].get();
  std::unique_ptr<rocksdb::Iterator> it{
      dbHandles_.db->NewIterator(ReadOptions(), columnFamily)};
  const WriteOptions writeOptions;
  for (it->SeekToFirst(); it->Valid(); it->Next()) {
    dbHandles_.db->Delete(writeOptions, columnFamily, it->key());
  }
}

void RocksDbLocalStore::compactKeySpace(KeySpace keySpace) {
  auto options = rocksdb::CompactRangeOptions{};
  options.allow_write_stall = true;
  auto columnFamily = dbHandles_.columns[keySpace].get();
  dbHandles_.db->CompactRange(
      options, columnFamily, /*begin=*/nullptr, /*end=*/nullptr);
}

StoreResult RocksDbLocalStore::get(LocalStore::KeySpace keySpace, ByteRange key)
    const {
  string value;
  auto status = dbHandles_.db->Get(
      ReadOptions(),
      dbHandles_.columns[keySpace].get(),
      _createSlice(key),
      &value);
  if (!status.ok()) {
    if (status.IsNotFound()) {
      // Return an empty StoreResult
      return StoreResult();
    }

    // TODO: RocksDB can return a "TryAgain" error.
    // Should we try again for the user, rather than re-throwing the error?

    // We don't use RocksException::check(), since we don't want to waste our
    // time computing the hex string of the key if we succeeded.
    throw RocksException::build(
        status, "failed to get ", folly::hexlify(key), " from local store");
  }
  return StoreResult(std::move(value));
}

FOLLY_NODISCARD folly::Future<StoreResult> RocksDbLocalStore::getFuture(
    KeySpace keySpace,
    folly::ByteRange key) const {
  // We're really just passing key through to the get() method, but we need to
  // make a copy of it on the way through.  It will usually be an eden::Hash
  // but can potentially be an arbitrary length so we can't just use Hash as
  // the storage here.  std::string is appropriate, but there's some noise
  // with the conversion from unsigned/signed and back again.
  return faultInjector_.checkAsync("local store get single", "")
      .via(&ioPool_)
      .thenValue([keySpace,
                  key = std::string(
                      reinterpret_cast<const char*>(key.data()), key.size()),
                  this](folly::Unit&&) {
        return get(
            keySpace,
            folly::ByteRange(
                reinterpret_cast<const unsigned char*>(key.data()),
                key.size()));
      });
}

FOLLY_NODISCARD folly::Future<std::vector<StoreResult>>
RocksDbLocalStore::getBatch(
    KeySpace keySpace,
    const std::vector<folly::ByteRange>& keys) const {
  std::vector<folly::Future<std::vector<StoreResult>>> futures;

  std::vector<std::shared_ptr<std::vector<std::string>>> batches;
  batches.emplace_back(std::make_shared<std::vector<std::string>>());

  for (auto& key : keys) {
    if (batches.back()->size() >= 2048) {
      batches.emplace_back(std::make_shared<std::vector<std::string>>());
    }
    batches.back()->emplace_back(
        reinterpret_cast<const char*>(key.data()), key.size());
  }

  for (auto& batch : batches) {
    futures.emplace_back(
        faultInjector_.checkAsync("local store get batch", "")
            .via(&ioPool_)
            .thenValue(
                [this, keySpace, keys = std::move(batch)](folly::Unit&&) {
                  XLOG(INFO) << __func__ << " starting to actually do work";
                  std::vector<Slice> keySlices;
                  std::vector<std::string> values;
                  std::vector<rocksdb::ColumnFamilyHandle*> columns;
                  for (auto& key : *keys) {
                    keySlices.emplace_back(key);
                    columns.emplace_back(dbHandles_.columns[keySpace].get());
                  }
                  auto statuses = dbHandles_.db->MultiGet(
                      ReadOptions(), columns, keySlices, &values);

                  std::vector<StoreResult> results;
                  for (size_t i = 0; i < keys->size(); ++i) {
                    auto& status = statuses[i];
                    if (!status.ok()) {
                      if (status.IsNotFound()) {
                        // Return an empty StoreResult
                        results.emplace_back(); // StoreResult();
                        continue;
                      }

                      // TODO: RocksDB can return a "TryAgain" error.
                      // Should we try again for the user, rather than
                      // re-throwing the error?

                      // We don't use RocksException::check(), since we don't
                      // want to waste our time computing the hex string of the
                      // key if we succeeded.
                      throw RocksException::build(
                          status,
                          "failed to get ",
                          folly::hexlify(keys->at(i)),
                          " from local store");
                    }
                    results.emplace_back(std::move(values[i]));
                  }
                  return results;
                }));
  }

  return folly::collect(futures).thenValue(
      [](std::vector<std::vector<StoreResult>>&& tries) {
        std::vector<StoreResult> results;
        for (auto& batch : tries) {
          results.insert(
              results.end(),
              make_move_iterator(batch.begin()),
              make_move_iterator(batch.end()));
        }
        return results;
      });
}

bool RocksDbLocalStore::hasKey(
    LocalStore::KeySpace keySpace,
    folly::ByteRange key) const {
  string value;
  auto status = dbHandles_.db->Get(
      ReadOptions(),
      dbHandles_.columns[keySpace].get(),
      _createSlice(key),
      &value);
  if (!status.ok()) {
    if (status.IsNotFound()) {
      return false;
    }

    // TODO: RocksDB can return a "TryAgain" error.
    // Should we try again for the user, rather than re-throwing the error?

    // We don't use RocksException::check(), since we don't want to waste our
    // time computing the hex string of the key if we succeeded.
    throw RocksException::build(
        status, "failed to get ", folly::hexlify(key), " from local store");
  }
  return true;
}

std::unique_ptr<LocalStore::WriteBatch> RocksDbLocalStore::beginWrite(
    size_t bufSize) {
  return std::make_unique<RocksDbWriteBatch>(dbHandles_, bufSize);
}

void RocksDbLocalStore::put(
    LocalStore::KeySpace keySpace,
    folly::ByteRange key,
    folly::ByteRange value) {
  dbHandles_.db->Put(
      WriteOptions(),
      dbHandles_.columns[keySpace].get(),
      _createSlice(key),
      _createSlice(value));
}

void RocksDbLocalStore::writeColumnFamilyIDs() {
  // Add an "id" entry to each column family, and then explicitly flush each
  // column family.
  //
  // The main purpose of this is to ensure the column family names have actually
  // been flushed out to SST files.  Otherwise the RocksDB RepairDB() function
  // can end up incorrectly dropping column families if they have never had any
  // data flushed to them.  (https://github.com/facebook/rocksdb/issues/5073)
  //
  // We can potentially use this key as a data format version number in case we
  // ever want to change the data format.  We could bump the value stored in
  // this field and then use it when opening a DB to tell if it is using the
  // new or old data format.
  rocksdb::WriteBatch batch;
  for (const auto& columnFamily : dbHandles_.columns) {
    // This key currently can never collide with keys for normal stored data.
    // All our column families currently use 20-byte hashes for their normal
    // keys.
    auto status =
        dbHandles_.db->Put(WriteOptions(), columnFamily.get(), "id", "1");
    if (!status.ok()) {
      throw RocksException::build(
          status, "error writing column family IDs to local store");
    }
    status = dbHandles_.db->Flush(FlushOptions(), columnFamily.get());
    if (!status.ok()) {
      throw RocksException::build(
          status, "error flushing column family IDs to local store");
    }
  }
}

} // namespace eden
} // namespace facebook
