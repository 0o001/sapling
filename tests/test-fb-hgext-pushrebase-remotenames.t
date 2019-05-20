TODO: Make this test compatibile with obsstore enabled.
  $ setconfig experimental.evolution=
Setup

  $ setconfig experimental.bundle2-exp=True

  $ cat >> $HGRCPATH << EOF
  > [ui]
  > ssh = python "$RUNTESTDIR/dummyssh"
  > EOF

Set up server repository

  $ hg init server
  $ cd server
  $ cat >> .hg/hgrc << EOF
  > [extensions]
  > pushrebase=
  > remotenames = !
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ echo foo > a
  $ echo foo > b
  $ hg commit -Am 'initial'
  adding a
  adding b
  $ hg book master
  $ cd ..

Set up client repository

  $ hg clone --config 'extensions.remotenames=' ssh://user@dummy/server client -q
  $ cp -R server server1
  $ hg clone --config 'extensions.remotenames=' ssh://user@dummy/server1 client1 -q

Test that pushing to a remotename preserves commit hash if no rebase happens

  $ cd client1
  $ setconfig extensions.remotenames= extensions.pushrebase=
  $ hg up -q master
  $ echo x >> a && hg commit -qm 'add a'
  $ hg commit --amend -qm 'changed message'
  $ hg log -r . -T '{node}\n'
  a4f02306629b883c3499865b4c0f1312743a15ca
  $ hg push --to master
  pushing rev a4f02306629b to destination ssh://user@dummy/server1 bookmark master
  searching for changes
  remote: pushing 1 changeset:
  remote:     a4f02306629b  changed message
  updating bookmark master
  $ hg log -r . -T '{node}\n'
  a4f02306629b883c3499865b4c0f1312743a15ca
  $ cd ..

Test that pushing to a remotename gets rebased

  $ cd server
  $ hg up -q master
  $ echo x >> a && hg commit -m "master's commit"
  $ cd ../client
  $ cat >> .hg/hgrc << EOF
  > [extensions]
  > remotenames =
  > pushrebase=
  > [remotenames]
  > allownonfastforward=True
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ echo x >> b && hg commit -m "client's commit"
  $ hg log -G -T '{rev} "{desc}" {remotebookmarks}'
  @  1 "client's commit"
  |
  o  0 "initial" default/master
  

  $ hg push --to master
  pushing rev 5c3cfb78df2f to destination ssh://user@dummy/server bookmark master
  searching for changes
  remote: pushing 1 changeset:
  remote:     5c3cfb78df2f  client's commit
  remote: 2 new changesets from the server will be downloaded
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 1 changes to 2 files (+1 heads)
  updating bookmark master

  $ hg log -G -T '{rev} "{desc}" {remotebookmarks}'
  o  3 "client's commit" default/master
  |
  o  2 "master's commit"
  |
  | @  1 "client's commit"
  |/
  o  0 "initial"
  

  $ cd ../server
  $ hg log -G -T '{rev} "{desc}" {bookmarks}'
  o  2 "client's commit" master
  |
  @  1 "master's commit"
  |
  o  0 "initial"
  
Test pushing a new bookmark
  $ cd ..
  $ hg -R client push --to newbook
  pushing rev 5c3cfb78df2f to destination ssh://user@dummy/server bookmark newbook
  searching for changes
  abort: not creating new remote bookmark
  (use --create to create a new bookmark)
  [255]

  $ hg -R client push --to newbook --create
  pushing rev 5c3cfb78df2f to destination ssh://user@dummy/server bookmark newbook
  searching for changes
  remote: pushing 1 changeset:
  remote:     5c3cfb78df2f  client's commit
  exporting bookmark newbook
  $ hg -R server book
   * master                    2:796d44dcaae0
     newbook                   3:5c3cfb78df2f
  $ hg -R server log -G -T '{rev} "{desc}" {bookmarks}'
  o  3 "client's commit" newbook
  |
  | o  2 "client's commit" master
  | |
  | @  1 "master's commit"
  |/
  o  0 "initial"
  
Test doing a non-fastforward bookmark move

  $ hg -R client push --to newbook -r master -f
  pushing rev 796d44dcaae0 to destination ssh://user@dummy/server bookmark newbook
  searching for changes
  no changes found
  updating bookmark newbook
  [1]
  $ hg -R server log -G -T '{rev} "{desc}" {bookmarks}'
  o  3 "client's commit"
  |
  | o  2 "client's commit" master newbook
  | |
  | @  1 "master's commit"
  |/
  o  0 "initial"
  

Test a push that comes with out-of-date bookmark discovery

  $ hg -R server debugstrip -q 0
  $ hg -R client debugstrip -q 0
  $ rm server/.hg/bookmarks*
  $ rm client/.hg/bookmarks*
  $ echo a >> server/a
  $ hg -R server commit -qAm 'aa'
  $ hg -R server bookmark bm -i
  $ echo b >> server/b
  $ hg -R server commit -qAm 'bb'
  $ hg -R server log -G -T '{rev} "{desc}" {bookmarks}'
  @  1 "bb"
  |
  o  0 "aa" bm
  

  $ cat >> $TESTTMP/move.py <<EOF
  > def movebookmark(ui, repo, **kwargs):
  >     import traceback
  >     if [f for f in traceback.extract_stack(limit=10)[:-1] if f[2] == "movebookmark"]:
  >         return
  >     import edenscm.mercurial.lock as lockmod
  >     tr = None
  >     try:
  >         lock = repo.lock()
  >         tr = repo.transaction("pretxnopen.movebook")
  >         changes = [('bm', repo[1].node())]
  >         repo._bookmarks.applychanges(repo, tr, changes)
  >         tr.close()
  >     finally:
  >         if tr:
  >             tr.release()
  >         lockmod.release(lock)
  >     print "moved bookmark to rev 1"
  > EOF
  $ cat >> server/.hg/hgrc <<EOF
  > [hooks]
  > pretxnopen.movebook = python:$TESTTMP/move.py:movebookmark
  > EOF
  $ hg -R client pull -q -r 0
  $ hg -R client update -q 0
  $ echo c >> client/c
  $ hg -R client commit -qAm 'cc'
  $ hg -R client log -G -T '{rev} "{desc}" {bookmarks}'
  @  1 "cc"
  |
  o  0 "aa"
  
  $ hg -R client push --to bm
  pushing rev 5db65b93a12b to destination ssh://user@dummy/server bookmark bm
  searching for changes
  remote: moved bookmark to rev 1
  remote: pushing 1 changeset:
  remote:     5db65b93a12b  cc
  remote: 2 new changesets from the server will be downloaded
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 1 changes to 2 files (+1 heads)
  updating bookmark bm
  $ hg -R server log -G -T '{rev} "{desc}" {bookmarks}'
  o  2 "cc" bm
  |
  @  1 "bb"
  |
  o  0 "aa"
  
  $ hg -R client log -G -T '{rev} "{desc}" {bookmarks}'
  o  3 "cc"
  |
  o  2 "bb"
  |
  | @  1 "cc"
  |/
  o  0 "aa"
  

Test that we still don't allow non-ff bm changes

  $ echo d > client/d
  $ hg -R client commit -qAm "dd"
  $ hg -R client log -G -T '{rev} "{desc}" {bookmarks}'
  @  4 "dd"
  |
  | o  3 "cc"
  | |
  | o  2 "bb"
  | |
  o |  1 "cc"
  |/
  o  0 "aa"
  

  $ hg -R client push --to bm
  pushing rev efec53e7b035 to destination ssh://user@dummy/server bookmark bm
  searching for changes
  remote: moved bookmark to rev 1
  remote: pushing 2 changesets:
  remote:     5db65b93a12b  cc
  remote:     efec53e7b035  dd
  remote: 1 new changeset from the server will be downloaded
  remote: transaction abort!
  remote: rollback completed
  abort: updating bookmark bm failed!
  [255]

Test force pushes
  $ hg init forcepushserver
  $ cd forcepushserver
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=
  > remotenames = !
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ echo a > a && hg commit -Aqm a
  $ hg book master
  $ cd ..

  $ hg clone -q --config 'extensions.remotenames=' ssh://user@dummy/forcepushserver forcepushclient
  $ cd forcepushserver
  $ echo a >> a && hg commit -Aqm aa

  $ cd ../forcepushclient
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=
  > remotenames =
  > [remotenames]
  > allownonfastforward=True
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ hg up master
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ echo b >> a && hg commit -Aqm b
  $ hg push -f --to master
  pushing rev 1846eede8b68 to destination * (glob)
  searching for changes
  remote: pushing 1 changeset:
  remote:     1846eede8b68  b
  updating bookmark master
  $ hg pull
  pulling from * (glob)
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files (+1 heads)
  new changesets 86cf3bb05fcf
  $ hg log -G -T '{rev} {desc} {remotebookmarks}'
  o  2 aa
  |
  | @  1 b default/master
  |/
  o  0 a
  
  $ cd ..

Test 'hg push' with a tracking bookmark
  $ hg init trackingserver
  $ cd trackingserver
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=
  > remotenames = !
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ echo a > a && hg commit -Aqm a
  $ hg book master
  $ cd ..
  $ hg clone --config 'extensions.remotenames=' -q ssh://user@dummy/trackingserver trackingclient
  $ cd trackingclient
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=
  > remotenames =
  > [remotenames]
  > allownonfastforward=True
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ hg book feature -t default/master
  $ echo b > b && hg commit -Aqm b
  $ cd ../trackingserver
  $ echo c > c && hg commit -Aqm c
  $ cd ../trackingclient
  $ hg push
  pushing rev d2ae7f538514 to destination ssh://user@dummy/trackingserver bookmark master
  searching for changes
  remote: pushing 1 changeset:
  remote:     d2ae7f538514  b
  remote: 2 new changesets from the server will be downloaded
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 1 changes to 2 files (+1 heads)
  updating bookmark master
  $ hg log -T '{rev} {desc}' -G
  o  3 b
  |
  o  2 c
  |
  | @  1 b
  |/
  o  0 a
  
  $ cd ..

Test push --to to a repo without pushrebase on (i.e. the default remotenames behavior)
  $ hg init oldserver
  $ cd oldserver
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > remotenames =
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ echo a > a && hg commit -Aqm a
  $ hg book serverfeature
  $ cd ..
  $ hg clone --config 'extensions.remotenames=' -q ssh://user@dummy/oldserver newclient
  $ cd newclient
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=
  > remotenames =
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ hg book clientfeature -t default/serverfeature
  $ echo b > b && hg commit -Aqm b
  $ hg push --to serverfeature
  pushing rev d2ae7f538514 to destination ssh://user@dummy/oldserver bookmark serverfeature
  searching for changes
  remote: adding changesets
  remote: adding manifests
  remote: adding file changes
  remote: added 1 changesets with 1 changes to 1 files
  updating bookmark serverfeature
  $ hg log -G -T '{shortest(node)} {bookmarks}'
  @  d2ae clientfeature
  |
  o  cb9a
  
  $ cd ../oldserver
  $ hg log -G -T '{shortest(node)} {bookmarks}'
  o  d2ae serverfeature
  |
  @  cb9a
  
Test push --to with remotenames but without pushrebase to a remote repository
that requires pushrebase.

  $ cd ..
  $ hg init pushrebaseserver
  $ cd pushrebaseserver
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > remotenames =
  > pushrebase=
  > [experimental]
  > bundle2-exp=True
  > [pushrebase]
  > blocknonpushrebase = True
  > EOF
  $ echo a > a && hg commit -Aqm a
  $ hg book serverfeature
  $ cd ..
  $ hg clone --config 'extensions.remotenames=' -q ssh://user@dummy/pushrebaseserver remotenamesonlyclient
  $ cd remotenamesonlyclient
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase=!
  > remotenames =
  > [experimental]
  > bundle2-exp=True
  > EOF
  $ hg book clientfeature -t default/serverfeature
  $ echo b > b && hg commit -Aqm b
  $ hg push --to serverfeature
  pushing rev d2ae7f538514 to destination ssh://user@dummy/pushrebaseserver bookmark serverfeature
  searching for changes
  remote: error: prechangegroup.blocknonpushrebase hook failed: this repository requires that you enable the pushrebase extension and push using 'hg push --to'
  remote: this repository requires that you enable the pushrebase extension and push using 'hg push --to'
  abort: push failed on remote
  [255]

