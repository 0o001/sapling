  $ . "$TESTDIR/library.sh"

# Populate the db with an initial commit

  $ initclient client
  $ cd client
  $ echo x > x
  $ hg commit -qAm x
  $ cd ..

  $ initserver master masterrepo
  $ cd master
  $ printf '[phases]\npublish=True\n' >> .hg/hgrc
  $ hg log
  $ hg pull -q ../client

  $ cd ..

# Verify local pushes work

  $ cd client
  $ echo y > y
  $ hg commit -qAm y
  $ hg phase -p -r 'all()'
  $ hg push ../master --traceback
  pushing to ../master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files

# Verify local pulls work
  $ hg strip -q -r tip
  $ hg pull ../master
  pulling from ../master
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  (run 'hg update' to get a working copy)
  $ hg log --template '{rev} {desc}\n'
  1 y
  0 x

# Verify local bookmark pull

  $ cd ../master
  $ hg book foo -r 0
  $ hg book
     foo                       0:b292c1e3311f
  $ cd ../client
  $ hg pull -q ../master
  $ hg book
     foo                       0:b292c1e3311f

# Verify local bookmark push

  $ hg book -r tip foo
  moving bookmark 'foo' forward from b292c1e3311f
  $ hg push ../master
  pushing to ../master
  searching for changes
  no changes found
  updating bookmark foo
  [1]
  $ hg book -R ../master
     foo                       1:d34c38483be9

# Verify explicit bookmark pulls work

  $ hg up tip
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ echo z > z
  $ hg commit -qAm z
  $ hg book foo
  moving bookmark 'foo' forward from d34c38483be9
  $ cd ../master
  $ hg pull -B foo ../client
  pulling from ../client
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  updating bookmark foo
  (run 'hg update' to get a working copy)
  $ hg log -l 1 --template '{rev} {bookmarks}\n'
  2 foo

# Push from hgsql to other repo

  $ hg up -q tip
  $ echo zz > z
  $ hg commit -m z2
  $ hg push ../client
  pushing to ../client
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
