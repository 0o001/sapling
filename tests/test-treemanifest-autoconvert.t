  $ . "$TESTDIR/library.sh"

  $ PYTHONPATH=$TESTDIR/..:$PYTHONPATH
  $ export PYTHONPATH

  $ hginit master
  $ cd master
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > server=True
  > EOF
  $ echo x > x
  $ hg commit -qAm 'add x'
  $ cd ..

  $ hgcloneshallow ssh://user@dummy/master client -q
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over * (glob)
  $ cd client
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > fastmanifest=
  > treemanifest=
  > 
  > [remotefilelog]
  > usefastdatapack=True
  > 
  > [treemanifest]
  > autocreatetrees=True
  > 
  > [fastmanifest]
  > usetree=True
  > usecache=False
  > EOF

Test auto creating trees for merge commit
  $ cd ../master
  $ hg up -q null
  $ echo z >> z
  $ hg commit -qAm 'add z'
  $ hg up -q 0
  $ hg merge -q -r 1
  $ hg commit -qAm 'merge'

  $ cd ../client
  $ hg pull
  pulling from ssh://user@dummy/master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 0 changes to 0 files
  (run 'hg update' to get a working copy)
  $ hg manifest -r tip
  x
  z
  $ hg debughistorypack $TESTTMP/hgcache/master/packs/manifests/5efcf6ea7ccfd8b6ac303c0f6b5a9cd73682e295
  
  
  Node          P1 Node       P2 Node       Link Node     Copy From
  13532a598745  bc0c2c938b92  000000000000  92f4ca0e667c  
  ddb35f099a64  000000000000  000000000000  d32fd17cb041  
