# no-check-code -- see T24862348

import os
import sys

from mercurial import hg

# wrapped in a try/except because of weirdness in how
# run.py works as compared to nose.
try:
    import test_hgsubversion_util
except ImportError:
    sys.path.insert(0, os.path.dirname(os.path.dirname(__file__)))
    import test_hgsubversion_util

from hgext.hgsubversion import svnwrap


def _do_case(self, name, stupid):
    subdir = test_hgsubversion_util.subdir.get(name, '')
    config = {
        'hgsubversion.stupid': stupid and '1' or '0',
        }
    repo, repo_path = self.load_and_fetch(name,
                                          subdir=subdir,
                                          layout='auto',
                                          config=config)
    assert test_hgsubversion_util.repolen(self.repo) > 0, \
        'Repo had no changes, maybe you need to add a subdir entry in test_hgsubversion_util?'
    wc2_path = self.wc_path + '_custom'
    checkout_path = repo_path
    if subdir:
        checkout_path += '/' + subdir
    u = test_hgsubversion_util.testui(stupid=stupid, layout='custom')
    for branch, path in test_hgsubversion_util.custom.get(name, {}).iteritems():
        u.setconfig('hgsubversionbranch', branch, path)
    test_hgsubversion_util.hgclone(u,
                      test_hgsubversion_util.fileurl(checkout_path),
                      wc2_path,
                      update=False)
    self.repo2 = hg.repository(test_hgsubversion_util.testui(), wc2_path)
    self.assertEqual(self.repo.heads(), self.repo2.heads())


def buildmethod(case, name, stupid):
    m = lambda self: self._do_case(case, stupid)
    m.__name__ = name
    replay = stupid and 'stupid' or 'regular'
    m.__doc__ = 'Test custom produces same as standard on %s. (%s)' % (case,
                                                                       replay)
    return m

attrs = {'_do_case': _do_case,
         }
for case in test_hgsubversion_util.custom:
    name = 'test_' + case[:-len('.svndump')].replace('-', '_')
    attrs[name] = buildmethod(case, name, stupid=False)
    if svnwrap.subversion_version < (1, 9, 0):
        name += '_stupid'
        attrs[name] = buildmethod(case, name, stupid=True)

CustomPullTests = type('CustomPullTests', (test_hgsubversion_util.TestBase,), attrs)
