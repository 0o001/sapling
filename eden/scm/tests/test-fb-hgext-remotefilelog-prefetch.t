  $ disable treemanifest
  $ setconfig experimental.allowfilepeer=True
  $ setconfig remotefilelog.write-hgcache-to-indexedlog=False remotefilelog.write-local-to-indexedlog=False
#testcases vfscachestore simplecachestore

  $ . "$TESTDIR/library.sh"

  $ hginit master
  $ cd master
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > server=True
  > EOF

#if simplecachestore
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > simplecacheserverstore=True
  > [extensions]
  > simplecache=
  > [simplecache]
  > cachedir=$TESTTMP/master/.hg/remotefilelogcache
  > caches=local
  > EOF
#endif

  $ echo x > x
  $ echo z > z
  $ hg commit -qAm x
  $ echo x2 > x
  $ echo y > y
  $ hg commit -qAm y
  $ hg bookmark foo

  $ cd ..

# prefetch a revision

  $ hgcloneshallow ssh://user@dummy/master shallow --noupdate
  streaming all changes
  3 files to transfer, * of data (glob)
  transferred * bytes in * seconds (*/sec) (glob)
  searching for changes
  no changes found
  $ cd shallow

  $ hg prefetch -r 0
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg cat -r 0 x
  x

# prefetch with base

  $ clearcache
  $ hg prefetch -r 0::1 -b 0
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg cat -r 1 x
  x2
  $ hg cat -r 1 y
  y

  $ hg cat -r 0 x
  x
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg cat -r 0 z
  z
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg prefetch -r 0::1 --base 0
  $ hg prefetch -r 0::1 -b 1
  $ hg prefetch -r 0::1

# prefetch a range of revisions

  $ clearcache
  $ hg prefetch -r 0::1
  4 files fetched over 1 fetches - (4 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg cat -r 0 x
  x
  $ hg cat -r 1 x
  x2

# prefetch certain files

  $ clearcache
  $ hg prefetch -r 1 x
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg cat -r 1 x
  x2

  $ hg cat -r 1 y
  y
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob) (?)

# prefetch on pull when configured

  $ printf "[remotefilelog]\npullprefetch=bookmark()\n" >> .hg/hgrc
  $ hg debugstrip tip

  $ clearcache
  $ hg pull
  pulling from ssh://user@dummy/master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  updating bookmark foo
  prefetching file contents
  3 files fetched over 1 fetches - (3 misses, 0.00% hit ratio) over *s (glob) (?)

  $ hg up tip
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved

# prefetch only fetches changes not in working copy

  $ hg debugstrip tip
  1 files updated, 0 files merged, 1 files removed, 0 files unresolved
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob) (?)
  $ clearcache

  $ hg pull
  pulling from ssh://user@dummy/master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  updating bookmark foo
  prefetching file contents
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob) (?)

# Make some local commits that produce the same file versions as are on the
# server. To simulate a situation where we have local commits that were somehow
# pushed, and we will soon pull.

  $ hg prefetch -r 'all()'
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob) (?)
  $ hg debugstrip -q -r 0
  $ echo x > x
  $ echo z > z
  $ hg commit -qAm x
  $ echo x2 > x
  $ echo y > y
  $ hg commit -qAm y

# prefetch server versions, even if local versions are available

  $ clearcache
  $ hg debugstrip -q tip
  $ hg pull
  pulling from ssh://user@dummy/master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  updating bookmark foo
  prefetching file contents
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob) (?)

  $ cd ..

# Prefetch unknown files during checkout

  $ hgcloneshallow ssh://user@dummy/master shallow2
  streaming all changes
  3 files to transfer, * of data (glob)
  transferred * bytes in * seconds * (glob)
  searching for changes
  no changes found
  updating to branch default
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over * (glob) (?)
  $ cd shallow2
  $ hg up -q null
  $ echo x > x
  $ echo y > y
  $ echo z > z
  $ clearcache
  $ hg up tip
  x: untracked file differs
  abort: untracked files in working directory differ from files in requested revision
  3 files fetched over 1 fetches - (3 misses, 0.00% hit ratio) over * (glob) (?)
  [255]
  $ hg revert --all

# Test batch fetching of lookup files during hg status
  $ hg up --clean tip
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg debugrebuilddirstate
  $ clearcache
  $ hg status
  3 files fetched over 1 fetches - (3 misses, 0.00% hit ratio) over * (glob) (?)

# Prefetch during addrename detection
  $ hg up -q --clean tip
  $ hg revert --all
  $ mv x x2
  $ mv y y2
  $ mv z z2
  $ clearcache
  $ hg addremove -s 50 > /dev/null
  * files fetched over 1 fetches - (* misses, 0.00% hit ratio) over * (glob) (?)

  $ cd ..

# Prefetch packs
  $ hgcloneshallow ssh://user@dummy/master packprefetch
  streaming all changes
  3 files to transfer, * of data (glob)
  transferred * bytes in * seconds (*/sec) (glob)
  searching for changes
  no changes found
  updating to branch default
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd packprefetch
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > fetchpacks=True
  > backgroundrepack=True
  > EOF
  $ clearcache
  $ hg prefetch -r .
  3 files fetched over 1 fetches - (3 misses, 0.00% hit ratio) over * (glob) (?)
  $ find $TESTTMP/hgcache/master/packs -type f | sort
  $TESTTMP/hgcache/master/packs/0a61bfbc8e0c4a08583b3f1abc7ad7f9cc9acc21.dataidx
  $TESTTMP/hgcache/master/packs/0a61bfbc8e0c4a08583b3f1abc7ad7f9cc9acc21.datapack
  $TESTTMP/hgcache/master/packs/47d8f1b90a73af4ff8af19fcd10bdc027b6a881a.histidx
  $TESTTMP/hgcache/master/packs/47d8f1b90a73af4ff8af19fcd10bdc027b6a881a.histpack
  $ hg cat -r . x
  x2
  $ hg cat -r . y
  y
  $ hg cat -r . z
  z

# Prefetch packs that include renames
  $ cd ../master
  $ hg mv z z2
  $ hg commit -m 'move z -> z2'
  $ cd ../packprefetch
  $ hg pull -q
  $ hg prefetch -r tip
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over * (glob) (?)
  $ hg up tip -q
  $ hg log -f z2 -T '{desc}\n'
  move z -> z2
  x

# check pulling renamed and changed file
  $ echo new_change >> z2
  $ hg commit -m "changed z2"
  $ hg push
  pushing to ssh://user@dummy/master
  searching for changes
  remote: adding changesets
  remote: adding manifests
  remote: adding file changes

  $ cd ..
  $ hgcloneshallow ssh://user@dummy/master packprefetch__2 --noupdate -q
  $ cd packprefetch__2
  $ hg prefetch -r tip -q
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over * (glob) (?)
  $ hg up tip -q
  $ cat z2
  z
  new_change

  $ cd ../packprefetch

# Revert across double renames. Note: the scary "abort", error is because
# https://bz.mercurial-scm.org/5419 .

  $ clearcache
  $ hg mv y y2
  $ hg mv x x2
  $ hg mv z2 z3
  $ hg revert -a -r 1 || true
  undeleting x
  forgetting x2
  undeleting y
  forgetting y2
  adding z
  forgetting z3
  abort: z2@109c3a557a73: not found in manifest! (?)
  3 files fetched over 1 fetches - (3 misses, 0.00% hit ratio) over * (glob) (?)

# Test connection pool lifetime
  $ clearcache
  $ hg prefetch -r 0::1 --debug --config connectionpool.lifetime=0 2>&1 | grep 'closing expired connection'
  4 files fetched over 1 fetches - (4 misses, 0.00% hit ratio) over * (glob) (?)
  closing expired connection to ssh://user@dummy/master
  $ clearcache
  $ hg prefetch -r 0::1 --debug --config connectionpool.lifetime=300 2>&1 | grep 'closing expired connection'
  4 files fetched over 1 fetches - (4 misses, 0.00% hit ratio) over * (glob) (?)
  [1]

  $ cat >$TESTTMP/testpool <<EOF
  > import time
  > with repo.connectionpool.get('ssh://user@dummy/master') as conn:
  >     connid = id(conn)
  >     repo.ui.debug("got first connection\n")
  > with repo.connectionpool.get('ssh://user@dummy/master') as conn:
  >     assert connid == id(conn)
  >     repo.ui.debug("got second connection\n")
  > time.sleep(2)
  > with repo.connectionpool.get('ssh://user@dummy/master') as conn:
  >     assert connid != id(conn)
  >     repo.ui.debug("got third connection\n")
  >     time.sleep(2)
  > EOF
  $ hg debugshell --command "`cat $TESTTMP/testpool`" --config connectionpool.lifetime=1 --debug 2>&1 | grep 'connection'
  got first connection
  reusing connection from pool
  got second connection
  not reusing expired connection to ssh://user@dummy/master
  got third connection
  closing expired connection to ssh://user@dummy/master
