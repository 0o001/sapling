# Copyright (c) 2016, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from __future__ import absolute_import
from __future__ import division
from __future__ import print_function
from __future__ import unicode_literals

import os
from .lib import gitrepo
from .lib import hgrepo
from .lib import testcase


class RepoTest(testcase.EdenTestCase):
    '''
    Tests for the "eden repository" command.

    Note that these tests do not use @testcase.eden_repo_test, since we don't
    actually need to run separately with git and mercurial repositories.  These
    tests don't actually mount anything in eden at all.
    '''
    def test_list_repository(self):
        self.assertEqual([], self._list_repos())

        config = '''\
[repository fbsource]
path = /data/users/carenthomas/fbsource
type = git

[bindmounts fbsource]
fbcode-buck-out = fbcode/buck-out
fbandroid-buck-out = fbandroid/buck-out
fbobjc-buck-out = fbobjc/buck-out
buck-out = buck-out

[repository git]
path = /home/carenthomas/src/git
type = git

[repository hg-crew]
url = /data/users/carenthomas/facebook-hg-rpms/hg-crew
type = hg
'''
        home_config_file = os.path.join(self.home_dir, '.edenrc')
        with open(home_config_file, 'w') as f:
            f.write(config)

        expected = ['fbsource', 'git', 'hg-crew']
        self.assertEqual(expected, self._list_repos())

    def test_add_multiple(self):
        hg_repo = self.create_repo('hg_repo', hgrepo.HgRepository)
        git_repo = self.create_repo('git_repo', gitrepo.GitRepository)

        self.eden.add_repository('hg1', hg_repo.path)
        self.assertEqual(['hg1'], self._list_repos())
        self.eden.add_repository('hg2', hg_repo.path)
        self.assertEqual(['hg1', 'hg2'], self._list_repos())
        self.eden.add_repository('git1', git_repo.path)
        self.assertEqual(['git1', 'hg1', 'hg2'], self._list_repos())

    def _list_repos(self):
        return self.eden.repository_cmd().splitlines()
