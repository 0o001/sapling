#chg-compatible

  $ disable treemanifest

Create a repository:

  $ setconfig format.use-segmented-changelog=1

  $ hg config
  devel.all-warnings=true
  devel.default-date=0 0
  experimental.metalog=true
  extensions.patchrmdir=
  extensions.fsmonitor= (fsmonitor !)
  extensions.treemanifest=!
  format.use-zstore-commit-data=false
  format.use-segmented-changelog=1
  fsmonitor.detectrace=1 (fsmonitor !)
  mutation.record=False
  remotefilelog.reponame=reponame-default
  remotefilelog.localdatarepack=True
  remotefilelog.cachepath=$TESTTMP/default-hgcache
  treemanifest.flatcompat=True
  treemanifest.sendtrees=False
  treemanifest.treeonly=False
  treemanifest.rustmanifest=True
  treemanifest.useruststore=True
  ui.slash=True
  ui.interactive=False
  ui.mergemarkers=detailed
  ui.promptecho=True
  web.address=localhost
  web\.ipv6=(?:True|False) (re)
  workingcopy.enablerustwalker=True
  $ hg init t
  $ cd t

Prepare a changeset:

  $ echo a > a
  $ hg add a

  $ hg status
  A a

Writes to stdio succeed and fail appropriately

#if devfull
  $ hg status 2>/dev/full
  A a

  $ hg status >/dev/full
  abort: No space left on device* (glob)
  [255]
#endif

Commands can succeed without a stdin
  $ hg log -r tip 0<&-
  commit:      000000000000
  user:        
  date:        Thu Jan 01 00:00:00 1970 +0000
  

#if devfull no-chg
  $ hg status >/dev/full 2>&1
  [1]

  $ hg status ENOENT 2>/dev/full
  [1]
#endif

#if devfull chg
  $ hg status >/dev/full 2>&1
  [255]

  $ hg status ENOENT 2>/dev/full
  [255]
#endif

  $ hg commit -m test

This command is ancient:

  $ hg history
  commit:      acb14030fe0a
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  summary:     test
  

Verify that updating to revision acb14030fe0a via commands.update() works properly

  $ cat <<EOF > update_to_rev0.py
  > from edenscm.mercurial import ui, hg, commands
  > myui = ui.ui.load()
  > repo = hg.repository(myui, path='.')
  > commands.update(myui, repo, rev='acb14030fe0a')
  > EOF
  $ hg up null
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ hg debugpython -- ./update_to_rev0.py
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg identify -n
  72057594037927936


Poke around at hashes:

  $ hg manifest --debug
  b789fdd96dc2f3bd229c1dd8eedf0fc60e2b68e3 644   a

  $ hg cat a
  a

Verify should succeed:

  $ hg verify
  verify for this repo format is not yet supported

At the end...

  $ cd ..
