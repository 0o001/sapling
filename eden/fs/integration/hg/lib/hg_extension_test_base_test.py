#!/usr/bin/env python3
#
# Copyright (c) 2016, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from .hg_extension_test_base import HgExtensionTestBase, EDEN_EXT_DIR
import os


class HgExtensionTestBaseTest(HgExtensionTestBase):
    '''Test to make sure that HgExtensionTestBase creates Eden mounts that are
    properly configured with the Hg extension.
    '''
    def populate_repo(self):
        self.repo.write_file('hello.txt', 'hola')
        self.repo.commit('Initial commit.')

    def test_setup(self):
        hg_dir = os.path.join(self.mount, '.hg')
        self.assertTrue(os.path.isdir(hg_dir))

        eden_extension = self.hg('config', 'extensions.eden').rstrip()
        self.assertEqual(EDEN_EXT_DIR, eden_extension)

        self.assertTrue(os.path.isfile(self.get_path('hello.txt')))
