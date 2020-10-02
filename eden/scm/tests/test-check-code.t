#chg-compatible

#require test-repo

  $ . "$TESTDIR/helpers-testrepo.sh"
  $ cd "$TESTDIR"/..

New errors are not allowed. Warnings are strongly discouraged.
(The writing "no-che?k-code" is for not skipping this file when checking.)

  $ testrepohg files . | egrep -v "^(edenscm/hgext/extlib/pywatchman|lib/cdatapack|lib/third-party|edenscm/mercurial/thirdparty|fb|newdoc|tests|edenscm/mercurial/templates/static|i18n|slides|.*\\.(bin|bindag|hg|pdf|jpg)$)" \
  > | sed 's-\\-/-g' > $TESTTMP/files.txt

  $ NPROC=`hg debugsh -c 'import multiprocessing; ui.write(str(multiprocessing.cpu_count()))'`
  $ cat $TESTTMP/files.txt | PYTHONPATH= xargs -n64 -P $NPROC contrib/check-code.py --warnings --per-file=0 | LC_ALL=C sort
  Skipping edenscm/hgext/extlib/cstore/key.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/cstore/store.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest.cpp it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_entry.cpp it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_entry.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_fetcher.cpp it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_fetcher.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_ptr.cpp it has no-che?k-code (glob)
  Skipping edenscm/hgext/extlib/ctreemanifest/manifest_ptr.h it has no-che?k-code (glob)
  Skipping edenscm/hgext/globalrevs.py it has no-che?k-code (glob)
  Skipping edenscm/hgext/hgsql.py it has no-che?k-code (glob)
  Skipping edenscm/mercurial/commands/eden.py it has no-che?k-code (glob)
  Skipping edenscm/mercurial/httpclient/__init__.py it has no-che?k-code (glob)
  Skipping edenscm/mercurial/httpclient/_readers.py it has no-che?k-code (glob)
  Skipping edenscm/mercurial/statprof.py it has no-che?k-code (glob)
  Skipping lib/clib/buffer.c it has no-che?k-code (glob)
  Skipping lib/clib/buffer.h it has no-che?k-code (glob)
  Skipping lib/clib/convert.h it has no-che?k-code (glob)
  Skipping lib/clib/null_test.c it has no-che?k-code (glob)
  Skipping lib/clib/portability/dirent.h it has no-che?k-code (glob)
  Skipping lib/clib/portability/inet.h it has no-che?k-code (glob)
  Skipping lib/clib/portability/mman.h it has no-che?k-code (glob)
  Skipping lib/clib/portability/portability.h it has no-che?k-code (glob)
  Skipping lib/clib/portability/unistd.h it has no-che?k-code (glob)
  Skipping lib/clib/sha1.h it has no-che?k-code (glob)
  edenscm/hgext/extlib/phabricator/graphql.py:*: use foobar, not foo_bar naming --> ca_bundle = repo.ui.configpath("web", "cacerts") (glob)
  edenscm/hgext/extlib/phabricator/graphql.py:*: use foobar, not foo_bar naming --> def scmquery_log( (glob)
  edenscm/hgext/hggit/git_handler.py:*: use foobar, not foo_bar naming --> git_renames = {} (glob)

@commands in debugcommands.py should be in alphabetical order.

  >>> import re
  >>> commands = []
  >>> with open('edenscm/mercurial/commands/debug.py', 'rb') as fh:
  ...     for line in fh:
  ...         m = re.match(b"^@command\('([a-z]+)", line)
  ...         if m:
  ...             commands.append(m.group(1))
  >>> scommands = list(sorted(commands))
  >>> for i, command in enumerate(scommands):
  ...     if command != commands[i]:
  ...         print('commands in debugcommands.py not sorted; first differing '
  ...               'command is %s; expected %s' % (commands[i], command))
  ...         break

