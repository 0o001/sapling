#chg-compatible

  $ . "$TESTDIR/library.sh"

  $ cat >> "$TESTTMP/uilog.py" <<EOF
  > from edenscm.mercurial import extensions
  > from edenscm.mercurial import ui as uimod
  > def uisetup(ui):
  >   extensions.wrapfunction(uimod.ui, 'log', mylog)
  > def mylog(orig, self, service, *msg, **opts):
  >   if service in ['undesired_file_fetches']:
  >     kw = []
  >     for k, v in sorted(opts.items()):
  >       kw.append("%s=%s" % (k, v))
  >     kwstr = ", ".join(kw)
  >     msgstr = msg[0] % msg[1:]
  >     self.warn('%s: %s (%s)\n' % (service, msgstr, kwstr))
  >     with open('$TESTTMP/undesiredfiles', 'a') as f:
  >       f.write('%s: %s (%s)\n' % (service, msgstr, kwstr))
  >   return orig(self, service, *msg, **opts)
  > EOF

  $ cat >> "$HGRCPATH" <<EOF
  > [extensions]
  > uilog=$TESTTMP/uilog.py
  > EOF

  $ newserver master
  $ clone master client1
  $ cd client1
  $ echo x > x
  $ hg commit -qAm x
  $ mkdir dir
  $ echo y > dir/y
  $ hg commit -qAm y
  $ hg push -r tip --to master --create
  pushing rev 79c51fb96423 to destination ssh://user@dummy/master bookmark master
  searching for changes
  exporting bookmark master
  remote: adding changesets (?)
  remote: adding manifests (?)
  remote: adding file changes (?)
  remote: added 2 changesets with 2 changes to 2 files (?)

  $ cd ..
  $ clone master shallow --noupdate
  $ cd shallow

  $ hg update -q master --config remotefilelog.undesiredfileregex=".*" 2>&1 | sort
  2 trees fetched over 0.00s
  fetching tree '' 05bd2758dd7a25912490d0633b8975bf52bfab06, found via 79c51fb96423
  undesired_file_fetches:  (filename=dir/y, reponame=master)
  undesired_file_fetches:  (filename=x, reponame=master)
