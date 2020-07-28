/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/journal/Journal.h"
#include <gmock/gmock.h>
#include <gtest/gtest.h>

using namespace facebook::eden;

TEST(Journal, accumulate_range_all_changes) {
  Journal journal(std::make_shared<EdenStats>());

  // Empty journals have no rang to accumulate over
  EXPECT_FALSE(journal.getLatest());
  EXPECT_EQ(nullptr, journal.accumulateRange());

  // Make an initial entry.
  journal.recordChanged("foo/bar"_relpath);

  // Sanity check that the latest information matches.
  auto latest = journal.getLatest();
  ASSERT_TRUE(latest);
  EXPECT_EQ(1, latest->sequenceID);

  // Add a second entry.
  journal.recordChanged("baz"_relpath);

  // Sanity check that the latest information matches.
  latest = journal.getLatest();
  ASSERT_TRUE(latest);
  EXPECT_EQ(2, latest->sequenceID);

  // Check basic sum implementation.
  auto summed = journal.accumulateRange();
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(1, summed->fromSequence);
  EXPECT_EQ(2, summed->toSequence);
  EXPECT_EQ(2, summed->changedFilesInOverlay.size());

  // First just report the most recent item.
  summed = journal.accumulateRange(2);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(2, summed->fromSequence);
  EXPECT_EQ(2, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());

  // Merge the first two entries.
  summed = journal.accumulateRange(1);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(1, summed->fromSequence);
  EXPECT_EQ(2, summed->toSequence);
  EXPECT_EQ(2, summed->changedFilesInOverlay.size());
}

TEST(Journal, accumulateRangeRemoveCreateUpdate) {
  Journal journal(std::make_shared<EdenStats>());

  // Remove test.txt
  journal.recordRemoved("test.txt"_relpath);
  // Create test.txt
  journal.recordCreated("test.txt"_relpath);
  // Modify test.txt
  journal.recordChanged("test.txt"_relpath);

  // Sanity check that the latest information matches.
  auto latest = journal.getLatest();
  ASSERT_TRUE(latest);
  EXPECT_EQ(3, latest->sequenceID);

  // The summed data should report test.txt as changed
  auto summed = journal.accumulateRange();
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(1, summed->fromSequence);
  EXPECT_EQ(3, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());
  ASSERT_EQ(1, summed->changedFilesInOverlay.count(RelativePath{"test.txt"}));
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedBefore);
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedAfter);

  // Test merging only partway back
  summed = journal.accumulateRange(3);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(3, summed->fromSequence);
  EXPECT_EQ(3, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());
  ASSERT_EQ(1, summed->changedFilesInOverlay.count(RelativePath{"test.txt"}));
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedBefore);
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedAfter);

  summed = journal.accumulateRange(2);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(2, summed->fromSequence);
  EXPECT_EQ(3, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());
  ASSERT_EQ(1, summed->changedFilesInOverlay.count(RelativePath{"test.txt"}));
  EXPECT_EQ(
      false,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedBefore);
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedAfter);

  summed = journal.accumulateRange(1);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(1, summed->fromSequence);
  EXPECT_EQ(3, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());
  ASSERT_EQ(1, summed->changedFilesInOverlay.count(RelativePath{"test.txt"}));
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedBefore);
  EXPECT_EQ(
      true,
      summed->changedFilesInOverlay[RelativePath{"test.txt"}].existedAfter);
}

void checkHashMatches(const Hash& from, const Hash& to, Journal& journal) {
  auto latest = journal.getLatest();
  ASSERT_TRUE(latest);
  EXPECT_EQ(from, latest->fromHash);
  EXPECT_EQ(to, latest->toHash);
  auto range = journal.accumulateRange(latest->sequenceID);
  ASSERT_TRUE(range);
  EXPECT_EQ(from, range->fromHash);
  EXPECT_EQ(to, range->toHash);
  range = journal.accumulateRange();
  ASSERT_TRUE(range);
  EXPECT_EQ(kZeroHash, range->fromHash);
  EXPECT_EQ(to, range->toHash);
}

TEST(Journal, accumulate_range_with_hash_updates) {
  Journal journal(std::make_shared<EdenStats>());

  auto hash0 = kZeroHash;
  auto hash1 = Hash("1111111111111111111111111111111111111111");
  auto hash2 = Hash("2222222222222222222222222222222222222222");
  // Empty journals have no range to accumulate over
  EXPECT_FALSE(journal.getLatest());
  EXPECT_EQ(nullptr, journal.accumulateRange());

  // Make an initial entry.
  journal.recordChanged("foo/bar"_relpath);
  checkHashMatches(hash0, hash0, journal);

  // Update to a new hash using 'to' syntax
  journal.recordHashUpdate(hash1);
  checkHashMatches(hash0, hash1, journal);

  journal.recordChanged("foo/bar"_relpath);
  checkHashMatches(hash1, hash1, journal);

  // Update to a new hash using 'from/to' syntax
  journal.recordHashUpdate(hash1, hash2);
  checkHashMatches(hash1, hash2, journal);

  journal.recordChanged("foo/bar"_relpath);
  checkHashMatches(hash2, hash2, journal);

  auto uncleanPaths = std::unordered_set<RelativePath>();
  uncleanPaths.insert(RelativePath("foo/bar"));
  journal.recordUncleanPaths(hash2, hash1, std::move(uncleanPaths));
  checkHashMatches(hash2, hash1, journal);

  journal.recordChanged("foo/bar"_relpath);
  checkHashMatches(hash1, hash1, journal);
}

TEST(Journal, debugRawJournalInfoRemoveCreateUpdate) {
  Journal journal(std::make_shared<EdenStats>());

  // Remove test.txt
  journal.recordRemoved("test.txt"_relpath);
  // Create test.txt
  journal.recordCreated("test.txt"_relpath);
  // Modify test.txt
  journal.recordChanged("test.txt"_relpath);

  long mountGen = 333;

  auto debugDeltas = journal.getDebugRawJournalInfo(0, 3, mountGen);
  ASSERT_EQ(3, debugDeltas.size());

  // Debug Raw Journal Info returns info from newest->latest
  EXPECT_TRUE(
      *debugDeltas[0].changedPaths_ref()["test.txt"].existedBefore_ref());
  EXPECT_TRUE(
      *debugDeltas[0].changedPaths_ref()["test.txt"].existedAfter_ref());
  EXPECT_EQ(
      *debugDeltas[0].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[0].fromPosition_ref()->sequenceNumber_ref(), 3);
  EXPECT_FALSE(
      *debugDeltas[1].changedPaths_ref()["test.txt"].existedBefore_ref());
  EXPECT_TRUE(
      *debugDeltas[1].changedPaths_ref()["test.txt"].existedAfter_ref());
  EXPECT_EQ(
      *debugDeltas[1].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[1].fromPosition_ref()->sequenceNumber_ref(), 2);
  EXPECT_TRUE(
      *debugDeltas[2].changedPaths_ref()["test.txt"].existedBefore_ref());
  EXPECT_FALSE(
      *debugDeltas[2].changedPaths_ref()["test.txt"].existedAfter_ref());
  EXPECT_EQ(
      *debugDeltas[2].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[2].fromPosition_ref()->sequenceNumber_ref(), 1);

  debugDeltas = journal.getDebugRawJournalInfo(0, 1, mountGen);
  ASSERT_EQ(1, debugDeltas.size());
  EXPECT_TRUE(
      *debugDeltas[0].changedPaths_ref()["test.txt"].existedBefore_ref());
  EXPECT_TRUE(
      *debugDeltas[0].changedPaths_ref()["test.txt"].existedAfter_ref());
  EXPECT_EQ(
      *debugDeltas[0].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[0].fromPosition_ref()->sequenceNumber_ref(), 3);

  debugDeltas = journal.getDebugRawJournalInfo(0, 0, mountGen);
  ASSERT_EQ(0, debugDeltas.size());
}

TEST(Journal, debugRawJournalInfoHashUpdates) {
  Journal journal(std::make_shared<EdenStats>());

  auto hash0 = kZeroHash;
  auto hash1 = Hash("1111111111111111111111111111111111111111");
  auto hash2 = Hash("2222222222222222222222222222222222222222");

  // Go from hash0 to hash1
  journal.recordHashUpdate(hash0, hash1);
  // Create test.txt
  journal.recordCreated("test.txt"_relpath);
  // Go from hash1 to hash2
  journal.recordHashUpdate(hash1, hash2);

  long mountGen = 333;

  auto debugDeltas = journal.getDebugRawJournalInfo(0, 3, mountGen);
  ASSERT_EQ(3, debugDeltas.size());

  // Debug Raw Journal Info returns info from newest->latest
  EXPECT_TRUE(debugDeltas[0].changedPaths_ref()->empty());
  EXPECT_EQ(
      *debugDeltas[0].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[0].fromPosition_ref()->sequenceNumber_ref(), 3);
  EXPECT_EQ(
      *debugDeltas[0].fromPosition_ref()->snapshotHash_ref(),
      thriftHash(hash1));
  EXPECT_EQ(
      *debugDeltas[0].toPosition_ref()->snapshotHash_ref(), thriftHash(hash2));
  EXPECT_FALSE(
      *debugDeltas[1].changedPaths_ref()["test.txt"].existedBefore_ref());
  EXPECT_TRUE(
      *debugDeltas[1].changedPaths_ref()["test.txt"].existedAfter_ref());
  EXPECT_EQ(
      *debugDeltas[1].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[1].fromPosition_ref()->sequenceNumber_ref(), 2);
  EXPECT_EQ(
      *debugDeltas[1].fromPosition_ref()->snapshotHash_ref(),
      thriftHash(hash1));
  EXPECT_EQ(
      *debugDeltas[1].toPosition_ref()->snapshotHash_ref(), thriftHash(hash1));
  EXPECT_TRUE(debugDeltas[2].changedPaths_ref()->empty());
  EXPECT_EQ(
      *debugDeltas[2].fromPosition_ref()->mountGeneration_ref(), mountGen);
  EXPECT_EQ(*debugDeltas[2].fromPosition_ref()->sequenceNumber_ref(), 1);
  EXPECT_EQ(
      *debugDeltas[2].fromPosition_ref()->snapshotHash_ref(),
      thriftHash(hash0));
  EXPECT_EQ(
      *debugDeltas[2].toPosition_ref()->snapshotHash_ref(), thriftHash(hash1));
}

TEST(Journal, destruction_does_not_overflow_stack_on_long_chain) {
  Journal journal(std::make_shared<EdenStats>());
  size_t N =
#ifdef NDEBUG
      200000 // Passes in under 200ms.
#else
      40000 // Passes in under 400ms.
#endif
      ;
  for (size_t i = 0; i < N; ++i) {
    journal.recordChanged("foo/bar"_relpath);
  }
}

TEST(Journal, empty_journal_returns_none_for_stats) {
  // Empty journal returns None for stats
  Journal journal(std::make_shared<EdenStats>());
  auto stats = journal.getStats();
  ASSERT_FALSE(stats.has_value());
}

TEST(Journal, basic_journal_stats) {
  Journal journal(std::make_shared<EdenStats>());
  // Journal with 1 entry
  journal.recordRemoved("test.txt"_relpath);
  ASSERT_TRUE(journal.getLatest());
  auto from1 = journal.getLatest()->time;
  auto to1 = journal.getLatest()->time;
  auto stats = journal.getStats();
  ASSERT_TRUE(stats.has_value());
  ASSERT_EQ(1, stats->entryCount);
  ASSERT_EQ(from1, stats->earliestTimestamp);
  ASSERT_EQ(to1, stats->latestTimestamp);

  // Journal with 2 entries
  journal.recordCreated("test.txt"_relpath);
  stats = journal.getStats();
  ASSERT_TRUE(journal.getLatest());
  auto to2 = journal.getLatest()->time;
  ASSERT_TRUE(stats.has_value());
  ASSERT_EQ(2, stats->entryCount);
  ASSERT_EQ(from1, stats->earliestTimestamp);
  ASSERT_EQ(to2, stats->latestTimestamp);
}

TEST(Journal, truncated_read_stats) {
  // Since each test is run on a single thread we can check that the stats of
  // this thread match up with what we would expect.
  auto edenStats = std::make_shared<EdenStats>();
  Journal journal(edenStats);
  journal.setMemoryLimit(0);
  journal.recordCreated("test1.txt"_relpath);
  journal.recordRemoved("test1.txt"_relpath);
  ASSERT_EQ(
      0, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
  // Empty Accumulate range, should be 0 files accumulated
  journal.accumulateRange(3);
  ASSERT_EQ(
      0, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
  // This is not a truncated read since journal remembers at least one entry
  journal.accumulateRange(2);
  ASSERT_EQ(
      0, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
  journal.accumulateRange(1);
  ASSERT_EQ(
      1, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
  journal.accumulateRange(2);
  ASSERT_EQ(
      1, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
  journal.accumulateRange(1);
  ASSERT_EQ(
      2, edenStats->getJournalStatsForCurrentThread().truncatedReads.sum());
}

TEST(Journal, files_accumulated_stats) {
  // Since each test is run on a single thread we can check that the stats of
  // this thread match up with what we would expect.
  auto edenStats = std::make_shared<EdenStats>();
  Journal journal(edenStats);
  journal.recordCreated("test1.txt"_relpath);
  journal.recordRemoved("test1.txt"_relpath);
  ASSERT_EQ(
      0, edenStats->getJournalStatsForCurrentThread().filesAccumulated.sum());
  ASSERT_EQ(0, journal.getStats()->maxFilesAccumulated);
  // Empty Accumulate range, should be 0 files accumulated
  journal.accumulateRange(3);
  ASSERT_EQ(
      0, edenStats->getJournalStatsForCurrentThread().filesAccumulated.sum());
  ASSERT_EQ(0, journal.getStats()->maxFilesAccumulated);
  journal.accumulateRange(2);
  ASSERT_EQ(
      1, edenStats->getJournalStatsForCurrentThread().filesAccumulated.sum());
  ASSERT_EQ(1, journal.getStats()->maxFilesAccumulated);
  journal.accumulateRange(1);
  ASSERT_EQ(
      3, edenStats->getJournalStatsForCurrentThread().filesAccumulated.sum());
  ASSERT_EQ(2, journal.getStats()->maxFilesAccumulated);
  journal.accumulateRange(2);
  ASSERT_EQ(
      4, edenStats->getJournalStatsForCurrentThread().filesAccumulated.sum());
  ASSERT_EQ(2, journal.getStats()->maxFilesAccumulated);
}

TEST(Journal, memory_usage) {
  Journal journal(std::make_shared<EdenStats>());
  auto stats = journal.getStats();
  uint64_t prevMem = journal.estimateMemoryUsage();
  for (int i = 0; i < 10; i++) {
    if (i % 2 == 0) {
      journal.recordCreated("test.txt"_relpath);
    } else {
      journal.recordRemoved("test.txt"_relpath);
    }
    stats = journal.getStats();
    uint64_t newMem = journal.estimateMemoryUsage();
    ASSERT_GT(newMem, prevMem);
    prevMem = newMem;
  }
}

TEST(Journal, set_get_memory_limit) {
  Journal journal(std::make_shared<EdenStats>());
  journal.setMemoryLimit(500);
  ASSERT_EQ(500, journal.getMemoryLimit());
  journal.setMemoryLimit(333);
  ASSERT_EQ(333, journal.getMemoryLimit());
  journal.setMemoryLimit(0);
  ASSERT_EQ(0, journal.getMemoryLimit());
}

TEST(Journal, truncation_by_flush) {
  Journal journal(std::make_shared<EdenStats>());
  journal.recordCreated("file1.txt"_relpath);
  journal.recordCreated("file2.txt"_relpath);
  journal.recordCreated("file3.txt"_relpath);
  auto summed = journal.accumulateRange(1);
  ASSERT_TRUE(summed);
  EXPECT_FALSE(summed->isTruncated);
  journal.flush();
  summed = journal.accumulateRange(1);
  ASSERT_TRUE(summed);
  EXPECT_TRUE(summed->isTruncated);
}

TEST(Journal, limit_of_zero_holds_one_entry) {
  Journal journal(std::make_shared<EdenStats>());
  // Even though limit is 0, journal will always remember at least one entry
  journal.setMemoryLimit(0);
  // With 1 file we should be able to accumulate from anywhere without
  // truncation, nullptr returned for sequenceID's > 1 (empty ranges)
  journal.recordCreated("file1.txt"_relpath);
  auto summed = journal.accumulateRange(1);
  ASSERT_TRUE(summed);
  EXPECT_FALSE(summed->isTruncated);
  summed = journal.accumulateRange(2);
  EXPECT_FALSE(summed);
}

TEST(Journal, limit_of_zero_truncates_after_one_entry) {
  Journal journal(std::make_shared<EdenStats>());
  // Even though limit is 0, journal will always remember at least one entry
  journal.setMemoryLimit(0);
  // With 2 files but only one entry in the journal we can only accumulate from
  // sequenceID 2 and above without truncation, nullptr returned for
  // sequenceID's > 2 (empty ranges)
  journal.recordCreated("file1.txt"_relpath);
  journal.recordCreated("file2.txt"_relpath);
  auto summed = journal.accumulateRange(1);
  ASSERT_TRUE(summed);
  EXPECT_TRUE(summed->isTruncated);
  summed = journal.accumulateRange(2);
  ASSERT_TRUE(summed);
  EXPECT_FALSE(summed->isTruncated);
  summed = journal.accumulateRange(3);
  EXPECT_FALSE(summed);
}

TEST(Journal, truncation_nonzero) {
  Journal journal(std::make_shared<EdenStats>());
  // Set the journal to a size such that it can store a few entries
  journal.setMemoryLimit(1500);
  int totalEntries = 0;
  int rememberedEntries;
  // Keep looping until we get a decent amount of truncation
  do {
    if (totalEntries % 2 == 0) {
      journal.recordCreated("file1.txt"_relpath);
    } else {
      journal.recordRemoved("file1.txt"_relpath);
    }
    ++totalEntries;
    rememberedEntries = journal.getStats()->entryCount;
    auto firstUntruncatedEntry = totalEntries - rememberedEntries + 1;
    for (int j = 1; j < firstUntruncatedEntry; j++) {
      auto summed = journal.accumulateRange(j);
      ASSERT_TRUE(summed);
      // If the value we are accumulating from is more than rememberedEntries
      // from the current sequenceID then it should be truncated
      EXPECT_TRUE(summed->isTruncated)
          << "Failed when remembering " << rememberedEntries
          << " entries out of " << totalEntries
          << " total entries with j = " << j;
    }
    for (int j = firstUntruncatedEntry; j <= totalEntries; j++) {
      auto summed = journal.accumulateRange(j);
      ASSERT_TRUE(summed);
      // If the value we are accumulating from is less than or equal to
      // rememberedEntries from the current sequenceID then it should not be
      // truncated
      EXPECT_FALSE(summed->isTruncated)
          << "Failed when remembering " << rememberedEntries
          << " entries out of " << totalEntries
          << " total entries with j = " << j;
    }
  } while (rememberedEntries + 5 > totalEntries);
}

TEST(Journal, compaction) {
  Journal journal(std::make_shared<EdenStats>());

  journal.recordCreated("file1.txt"_relpath);
  auto stats = journal.getStats();
  ASSERT_TRUE(stats.has_value());
  ASSERT_EQ(1, stats->entryCount);
  auto latest = journal.getLatest();
  ASSERT_TRUE(latest);
  ASSERT_EQ(1, latest->sequenceID);

  journal.recordChanged("file1.txt"_relpath);
  stats = journal.getStats();
  ASSERT_TRUE(stats.has_value());
  ASSERT_EQ(2, stats->entryCount);
  latest = journal.getLatest();
  ASSERT_TRUE(latest);
  ASSERT_EQ(2, latest->sequenceID);
  auto summed = journal.accumulateRange(2);
  ASSERT_NE(nullptr, summed);
  EXPECT_EQ(2, summed->fromSequence);
  EXPECT_EQ(2, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());

  // Changing file1.txt again should just change the sequenceID of the last
  // delta to be 3
  journal.recordChanged("file1.txt"_relpath);
  stats = journal.getStats();
  ASSERT_TRUE(stats.has_value());
  ASSERT_EQ(2, stats->entryCount);
  latest = journal.getLatest();
  ASSERT_TRUE(latest);
  ASSERT_EQ(3, latest->sequenceID);
  summed = journal.accumulateRange(2);
  ASSERT_NE(nullptr, summed);
  // We expect from to be 3 since there is no delta with sequence ID = 2
  EXPECT_EQ(3, summed->fromSequence);
  EXPECT_EQ(3, summed->toSequence);
  EXPECT_EQ(1, summed->changedFilesInOverlay.size());
}
