#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from .lib import testcase
import errno
import os
import shutil


@testcase.eden_repo_test
class UnlinkTest:
    def populate_repo(self):
        self.repo.write_file('hello', 'hola\n')
        self.repo.write_file('adir/file', 'foo!\n')
        self.repo.symlink('slink', 'hello')
        self.repo.commit('Initial commit.')

    def test_unlink(self):
        filename = os.path.join(self.mount, 'hello')

        # This file is part of the git repo
        with open(filename, 'r') as f:
            self.assertEqual('hola\n', f.read())

        # Removing should succeed
        os.unlink(filename)

        with self.assertRaises(OSError) as context:
            os.lstat(filename)
        self.assertEqual(context.exception.errno, errno.ENOENT,
                         msg='lstat on a removed file raises ENOENT')

    def test_unlink_bogus_file(self):
        with self.assertRaises(OSError) as context:
            os.unlink(os.path.join(self.mount, 'this-file-does-not-exist'))
        self.assertEqual(context.exception.errno, errno.ENOENT,
                         msg='unlink raises ENOENT for nonexistent file')

    def test_unlink_dir(self):
        adir = os.path.join(self.mount, 'adir')
        with self.assertRaises(OSError) as context:
            os.unlink(adir)
        self.assertEqual(context.exception.errno, errno.EISDIR,
                         msg='unlink on a dir raises EISDIR')

    def test_unlink_empty_dir(self):
        adir = os.path.join(self.mount, 'an-empty-dir')
        os.mkdir(adir)
        with self.assertRaises(OSError) as context:
            os.unlink(adir)
        self.assertEqual(context.exception.errno, errno.EISDIR,
                         msg='unlink on an empty dir raises EISDIR')

    def test_rmdir_file(self):
        filename = os.path.join(self.mount, 'hello')

        with self.assertRaises(OSError) as context:
            os.rmdir(filename)
        self.assertEqual(context.exception.errno, errno.ENOTDIR,
                         msg='rmdir on a file raises ENOTDIR')

    def test_rmdir(self):
        adir = os.path.join(self.mount, 'adir')
        with self.assertRaises(OSError) as context:
            os.rmdir(adir)
        self.assertEqual(context.exception.errno, errno.ENOTEMPTY,
                         msg='rmdir on a non-empty dir raises ENOTEMPTY')

        shutil.rmtree(adir)
        with self.assertRaises(OSError) as context:
            os.lstat(adir)
        self.assertEqual(context.exception.errno, errno.ENOENT,
                         msg='lstat on a removed dir raises ENOENT')

    def test_rmdir_overlay(self):
        # Ensure that removing dirs works with things we make in the overlay
        deep_root = os.path.join(self.mount, 'buck-out')
        deep_name = os.path.join(deep_root, 'foo', 'bar', 'baz')
        os.makedirs(deep_name)
        with self.assertRaises(OSError) as context:
            os.rmdir(deep_root)
        self.assertEqual(context.exception.errno, errno.ENOTEMPTY,
                         msg='rmdir on a non-empty dir raises ENOTEMPTY')

        shutil.rmtree(deep_root)
        with self.assertRaises(OSError) as context:
            os.lstat(deep_root)
        self.assertEqual(context.exception.errno, errno.ENOENT,
                         msg='lstat on a removed dir raises ENOENT')
