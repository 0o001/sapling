#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import errno
import os
import stat
from .lib import testcase
from facebook.eden import EdenService
from facebook.eden.ttypes import FileInformationOrError


INITIAL_SEQ = 5

@testcase.eden_repo_test
class MaterializedQueryTest:
    '''Check that materialization is represented correctly.'''

    def populate_repo(self):
        self.repo.write_file('hello', 'hola\n')
        self.repo.write_file('adir/file', 'foo!\n')
        self.repo.write_file('bdir/test.sh', '#!/bin/bash\necho test\n',
                             mode=0o755)
        self.repo.write_file('bdir/noexec.sh', '#!/bin/bash\necho test\n')
        self.repo.symlink('slink', 'hello')
        self.repo.commit('Initial commit.')

    def edenfs_logging_settings(self):
        return {'eden.fs.fuse.RequestData': 'DBG5'}

    def setUp(self):
        super().setUp()
        self.client = self.get_thrift_client()
        self.client.open()

    def tearDown(self):
        self.client.close()
        super().tearDown()

    def test_noEntries(self):
        pos = self.client.getCurrentJournalPosition(self.mount)
        # setting up the .eden dir bumps the sequence number
        self.assertEqual(INITIAL_SEQ, pos.sequenceNumber)
        self.assertNotEqual(0, pos.mountGeneration)

        changed = self.client.getFilesChangedSince(self.mount, pos)
        self.assertEqual(set(), set(changed.changedPaths))
        self.assertEqual(set(), set(changed.createdPaths))
        self.assertEqual(set(), set(changed.removedPaths))
        self.assertEqual(pos, changed.fromPosition)
        self.assertEqual(pos, changed.toPosition)

    def test_getFileInformation(self):
        """ verify that getFileInformation is consistent with the VFS """

        paths = ['', 'not-exist', 'hello', 'adir',
                 'adir/file', 'bdir/test.sh', 'slink']
        info_list = self.client.getFileInformation(self.mount, paths)
        self.assertEqual(len(paths), len(info_list))

        for idx, path in enumerate(paths):
            try:
                st = os.lstat(os.path.join(self.mount, path))
                self.assertEqual(FileInformationOrError.INFO, info_list[
                                 idx].getType(),
                                 msg='have non-error result for ' + path)
                info = info_list[idx].get_info()
                self.assertEqual(st.st_mode, info.mode,
                                 msg='mode matches for ' + path)
                self.assertEqual(st.st_size, info.size,
                                 msg='size matches for ' + path)
                self.assertEqual(int(st.st_mtime), info.mtime.seconds)
                if not stat.S_ISDIR(st.st_mode):
                    self.assertNotEqual(0, st.st_mtime)
                    self.assertNotEqual(0, st.st_ctime)
                    self.assertNotEqual(0, st.st_atime)
            except OSError as e:
                self.assertEqual(FileInformationOrError.ERROR, info_list[
                                 idx].getType(),
                                 msg='have error result for ' + path)
                err = info_list[idx].get_error()
                self.assertEqual(e.errno, err.errorCode,
                                 msg='error code matches for ' + path)

    def test_invalidProcessGeneration(self):
        # Get a candidate position
        pos = self.client.getCurrentJournalPosition(self.mount)

        # poke the generation to a value that will never manifest in practice
        pos.mountGeneration = 0

        with self.assertRaises(EdenService.EdenError) as context:
            self.client.getFilesChangedSince(self.mount, pos)
        self.assertEqual(errno.ERANGE, context.exception.errorCode,
                         msg='Must return ERANGE')

    def test_removeFile(self):
        initial_pos = self.client.getCurrentJournalPosition(self.mount)
        self.assertEqual(INITIAL_SEQ, initial_pos.sequenceNumber)

        os.unlink(os.path.join(self.mount, 'adir', 'file'))
        changed = self.client.getFilesChangedSince(self.mount, initial_pos)
        self.assertEqual(set(), set(changed.createdPaths))
        self.assertEqual(set(), set(changed.changedPaths))
        self.assertEqual(set(['adir/file']), set(changed.removedPaths))

    def test_renameFile(self):
        initial_pos = self.client.getCurrentJournalPosition(self.mount)
        self.assertEqual(INITIAL_SEQ, initial_pos.sequenceNumber)

        os.rename(os.path.join(self.mount, 'hello'), os.path.join(self.mount, 'bye'))
        changed = self.client.getFilesChangedSince(self.mount, initial_pos)
        self.assertEqual(set(['bye']), set(changed.createdPaths))
        self.assertEqual(set(), set(changed.changedPaths))
        self.assertEqual(set(['hello']), set(changed.removedPaths))

    def test_addFile(self):
        initial_pos = self.client.getCurrentJournalPosition(self.mount)
        self.assertEqual(INITIAL_SEQ, initial_pos.sequenceNumber)

        name = os.path.join(self.mount, 'overlaid')
        with open(name, 'w+') as f:
            pos = self.client.getCurrentJournalPosition(self.mount)
            self.assertEqual(INITIAL_SEQ + 1, pos.sequenceNumber,
                             msg='creating a file bumps the journal')

            changed = self.client.getFilesChangedSince(self.mount, initial_pos)
            self.assertEqual(set(['overlaid']), set(changed.createdPaths))
            self.assertEqual(set(), set(changed.changedPaths))
            self.assertEqual(set(), set(changed.removedPaths))
            self.assertEqual(initial_pos.sequenceNumber + 1,
                             changed.fromPosition.sequenceNumber,
                             msg='changes start AFTER initial_pos')

            f.write('NAME!\n')

        pos_after_overlaid = self.client.getCurrentJournalPosition(self.mount)
        self.assertEqual(INITIAL_SEQ + 2, pos_after_overlaid.sequenceNumber,
                         msg='writing bumps the journal')
        changed = self.client.getFilesChangedSince(self.mount, initial_pos)
        self.assertEqual(set(['overlaid']), set(changed.createdPaths))
        self.assertEqual(set(), set(changed.changedPaths))
        self.assertEqual(set(), set(changed.removedPaths))
        self.assertEqual(initial_pos.sequenceNumber + 1,
                         changed.fromPosition.sequenceNumber,
                         msg='changes start AFTER initial_pos')

        name = os.path.join(self.mount, 'adir', 'file')
        with open(name, 'a') as f:
            pos = self.client.getCurrentJournalPosition(self.mount)
            self.assertEqual(INITIAL_SEQ + 2, pos.sequenceNumber,
                             msg='journal still in same place for append')
            f.write('more stuff on the end\n')

        pos = self.client.getCurrentJournalPosition(self.mount)
        self.assertEqual(INITIAL_SEQ + 3, pos.sequenceNumber,
                         msg='appending bumps the journal')

        changed = self.client.getFilesChangedSince(
            self.mount, pos_after_overlaid)
        self.assertEqual(set(['adir/file']), set(changed.changedPaths))
        self.assertEqual(set(), set(changed.createdPaths))
        self.assertEqual(set(), set(changed.removedPaths))
        self.assertEqual(pos_after_overlaid.sequenceNumber + 1,
                         changed.fromPosition.sequenceNumber,
                         msg='changes start AFTER pos_after_overlaid')
