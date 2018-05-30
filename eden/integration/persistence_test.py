#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import os
import unittest
from typing import Dict

from .lib import testcase


@testcase.eden_repo_test
class PersistenceTest(testcase.EdenRepoTest):
    def populate_repo(self) -> None:
        self.repo.write_file("file_in_root", "contents1")
        self.repo.write_file("subdir/file_in_subdir", "contents2")
        self.repo.write_file("subdir2/file_in_subdir2", "contents3")
        self.repo.commit("Initial commit.")

    # These tests restart Eden and expect data to have persisted.
    def select_storage_engine(self) -> str:
        return "sqlite"

    def edenfs_logging_settings(self) -> Dict[str, str]:
        return {"eden.strace": "DBG7", "eden.fs.fuse": "DBG7"}

    # It is not a strict requirement that Eden always remember inode numbers
    # across restart -- we could theoretically drop them whenever we know it's
    # not sensible for a program to remember them across whatever event.
    #
    # However, today we remember metadata keyed on inode, and thus Eden does,
    # in practice remember them across restarts.
    def test_preserves_inode_numbers_across_restarts(self):
        before1 = os.lstat(os.path.join(self.mount, "subdir/file_in_subdir"))
        before2 = os.lstat(os.path.join(self.mount, "subdir2/file_in_subdir2"))

        self.eden.shutdown()
        self.eden.start()

        # stat in reverse order
        after2 = os.lstat(os.path.join(self.mount, "subdir2/file_in_subdir2"))
        after1 = os.lstat(os.path.join(self.mount, "subdir/file_in_subdir"))

        self.assertEqual(before1.st_ino, after1.st_ino)
        self.assertEqual(before2.st_ino, after2.st_ino)

    def test_preserves_nonmaterialized_inode_metadata(self) -> None:
        inode_paths = [
            "file_in_root",
            "subdir",
            "subdir/file_in_subdir",  # we care about trees too
        ]

        old_stats = [os.lstat(os.path.join(self.mount, path)) for path in inode_paths]

        self.eden.shutdown()
        self.eden.start()

        new_stats = [os.lstat(os.path.join(self.mount, path)) for path in inode_paths]

        for (path, old_stat, new_stat) in zip(inode_paths, old_stats, new_stats):
            self.assertEqual(
                old_stat.st_ino,
                new_stat.st_ino,
                f"inode numbers must line up for path {path}",
            )
            self.assertEqual(
                old_stat.st_mode, new_stat.st_mode, f"mode must line up for path {path}"
            )
            self.assertEqual(
                old_stat.st_atime,
                new_stat.st_atime,
                f"atime must line up for path {path}",
            )
            self.assertEqual(
                old_stat.st_mtime,
                new_stat.st_mtime,
                f"mtime must line up for path {path}",
            )
            self.assertEqual(
                old_stat.st_ctime,
                new_stat.st_ctime,
                f"ctime must line up for path {path}",
            )
