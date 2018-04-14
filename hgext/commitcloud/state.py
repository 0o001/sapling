# Copyright 2018 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

# Standard Library
import hashlib
import json

# Mercurial
from mercurial.i18n import _

from .. import shareutil
from . import (
    commitcloudcommon,
    commitcloudutil,
)

class SyncState(object):
    """
    Stores the local record of what state was stored in the cloud at the
    last sync.
    """
    @staticmethod
    def _filename(workspace):
        # make a unique valid filename
        return 'commitcloudstate.' + ''.join(
            x for x in workspace if x.isalnum()) + '.%s' % (
                hashlib.sha256(workspace).hexdigest()[0:5])

    @staticmethod
    def erasestate(repo):
        # get current workspace
        workspace = commitcloudutil.getworkspacename(repo)
        if not workspace:
            raise commitcloudcommon.WorkspaceError(
                repo.ui, _('undefined workspace'))

        filename = SyncState._filename(workspace)
        # clean up the current state in force recover mode
        if repo.svfs.exists(filename):
            with repo.wlock(), repo.lock():
                repo.svfs.unlink(filename)

    def __init__(self, repo):
        # get current workspace
        workspace = commitcloudutil.getworkspacename(repo)
        if not workspace:
            raise commitcloudcommon.WorkspaceError(
                repo.ui, _('undefined workspace'))

        self.filename = SyncState._filename(workspace)
        repo = shareutil.getsrcrepo(repo)
        self.repo = repo
        if repo.svfs.exists(self.filename):
            with repo.svfs.open(self.filename, 'r') as f:
                try:
                    data = json.load(f)
                except Exception:
                    raise commitcloudcommon.InvalidWorkspaceDataError(
                        repo.ui, _('failed to parse %s') % self.filename)

                self.version = data['version']
                self.heads = [h.encode() for h in data['heads']]
                self.bookmarks = {n.encode('utf-8'): v.encode()
                                  for n, v in data['bookmarks'].items()}
        else:
            self.version = 0
            self.heads = []
            self.bookmarks = {}

    def update(self, newversion, newheads, newbookmarks):
        data = {
            'version': newversion,
            'heads': newheads,
            'bookmarks': newbookmarks,
        }
        with self.repo.wlock(), self.repo.lock():
            with self.repo.svfs.open(self.filename, 'w', atomictemp=True) as f:
                json.dump(data, f)
        self.version = newversion
        self.heads = newheads
        self.bookmarks = newbookmarks
