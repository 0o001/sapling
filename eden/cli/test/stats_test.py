#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import unittest
from io import StringIO

from .. import stats_print
from ..stats import DiagInfoCounters, get_hg_importer_counters, get_store_latency


class StatsTest(unittest.TestCase):
    maxDiff = None

    def test_print_heading(self):
        expected_output = """\
                                   **********
                                   TheHeading
                                   **********

"""
        out = StringIO()
        stats_print.write_heading("TheHeading", out)
        self.assertEqual(out.getvalue(), expected_output)

    def test_print_latency_record(self):
        matrix = [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12], [13, 14, 15, 16]]
        expected_output = """\
              |      avg               1                2           3          4
              |      p50               5                6           7          8
access        |      p90               9               10          11         12
              |      p99              13               14          15         16
--------------------------------------------------------------------------------
"""

        out = StringIO()
        stats_print.write_latency_record("access", matrix, out)
        self.assertEqual(out.getvalue(), expected_output)

    def test_print_mem_status(self):
        dictionary = {
            "memory_free": 1234567,
            "memory_free_percent": 50,
            "memory_usage": 45678912,
            "memory_usage_percent": 70,
        }
        expected_output = """\
                             memory free : 1.234567(GB)
                     memory free percent : 50%
                            memory usage : 45.678912(GB)
                    memory usage percent : 70%
"""
        out = StringIO()
        stats_print.write_mem_status_table(dictionary, out)
        self.assertEqual(expected_output, out.getvalue())

    def test_print_table(self):
        table = {
            "key1": [1, 2, 3, 4],
            "key2": [5, 6, 7, 8],
            "key3": [9, 10, 11, 12],
            "key4": [13, 14, 15, 16],
        }
        expected_output = """\
SystemCall      Last Minute       Last 10m      Last Hour       All Time
------------------------------------------------------------------------
key1                      1              2              3              4
key2                      5              6              7              8
key3                      9             10             11             12
key4                     13             14             15             16
"""
        out = StringIO()
        stats_print.write_table(table, "SystemCall", out)
        self.assertEqual(expected_output, out.getvalue())

    def test_print_table_with_shorter_header_and_key_column(self):
        table = {"key": [1, 2, 3, 4]}
        # Verifies the width of the first column depends on the header's and
        # key's lengths.
        expected_output = """\
SC       Last Minute       Last 10m      Last Hour       All Time
-----------------------------------------------------------------
key                1              2              3              4
"""
        out = StringIO()
        stats_print.write_table(table, "SC", out)
        self.assertEqual(expected_output, out.getvalue())

    def test_format_size(self):
        self.assertEqual("1.5 GB", stats_print.format_size(1500000000))
        # rounds up
        self.assertEqual("1.6 GB", stats_print.format_size(1590000000))
        self.assertEqual("123.4 MB", stats_print.format_size(123400000))
        self.assertEqual("12 B", stats_print.format_size(12))
        self.assertEqual("0", stats_print.format_size(0))


class HgImporterStatsTest(unittest.TestCase):
    def test_call_counts_are_zero_if_no_data_was_logged(self) -> None:
        counters: DiagInfoCounters = {}
        table = get_hg_importer_counters(counters)
        metrics = [
            "cat_file",
            "fetch_tree",
            "manifest",
            "manifest_node_for_commit",
            "prefetch_files",
        ]
        for metric in metrics:
            self.assertEqual(
                table.get(metric), [0, 0, 0, 0], f"Metric {metric} should be zero"
            )

    def test_cat_file_call_counts_are_extracted_from_counters(self) -> None:
        counters: DiagInfoCounters = {
            "hg_importer.cat_file.count": 10,
            "hg_importer.cat_file.count.3600": 9,
            "hg_importer.cat_file.count.60": 1,
            "hg_importer.cat_file.count.600": 7,
        }
        table = get_hg_importer_counters(counters)
        self.assertEqual(table.get("cat_file"), [1, 7, 9, 10])

    def test_table_includes_unknown_counters(self) -> None:
        counters: DiagInfoCounters = {
            "hg_importer.dog_file.count": 100,
            "hg_importer.dog_file.count.3600": 90,
            "hg_importer.dog_file.count.60": 10,
            "hg_importer.dog_file.count.600": 70,
        }
        table = get_hg_importer_counters(counters)
        self.assertEqual(table.get("dog_file"), [10, 70, 90, 100])


class HgBackingStoreStatsTest(unittest.TestCase):
    def test_get_stats_from_right_store(self) -> None:
        counters: DiagInfoCounters = {
            "store.mononoke.get_blob.p50": 10,
            "store.mononoke.get_blob.p50.60": 20,
            "store.mononoke.get_blob.p50.600": 30,
            "store.mononoke.get_blob.p50.3600": 40,
            "store.hg.get_blob.p50": 40,
            "store.hg.get_blob.p50.60": 30,
            "store.hg.get_blob.p50.600": 20,
            "store.hg.get_blob.p50.3600": 10,
        }
        table = get_store_latency(counters, "mononoke")
        result = table.get("get_blob")
        if result:
            self.assertEqual(result[1], ["20 μs", "30 μs", "40 μs", "10 μs"])
        else:
            # make pyre happy
            self.assertTrue(False, "should return result")

    def test_get_store_latency_correctly(self) -> None:
        counters: DiagInfoCounters = {
            "store.mononoke.get_blob.count": 10,
            "store.mononoke.get_blob.count.60": 20,
            "store.mononoke.get_blob.count.600": 30,
            "store.mononoke.get_blob.count.3600": 40,
            "store.mononoke.get_blob.p50": 5010,
            "store.mononoke.get_blob.p50.60": 5020,
            "store.mononoke.get_blob.p50.600": 5030,
            "store.mononoke.get_blob.p50.3600": 5040,
            "store.mononoke.get_blob.p90": 9010,
            "store.mononoke.get_blob.p90.60": 9020,
            "store.mononoke.get_blob.p90.600": 9030,
            "store.mononoke.get_blob.p90.3600": 9040,
            "store.mononoke.get_blob.p99": 9910,
            "store.mononoke.get_blob.p99.60": 9920,
            "store.mononoke.get_blob.p99.600": 9930,
            "store.mononoke.get_blob.p99.3600": 9940,
        }
        table = get_store_latency(counters, "mononoke")
        self.assertEqual(
            table.get("get_blob"),
            [
                ["", "", "", ""],
                ["5020 μs", "5030 μs", "5040 μs", "5010 μs"],
                ["9020 μs", "9030 μs", "9040 μs", "9010 μs"],
                ["9920 μs", "9930 μs", "9940 μs", "9910 μs"],
            ],
        )
