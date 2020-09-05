#chg-compatible

  $ . "$TESTDIR/hgsql/library.sh"
  $ disable treemanifest

# Populate the db with an initial commit

  $ initclient client
  $ cd client
  $ echo x > x
  $ hg commit -qAm x
  $ echo y > y
  $ hg commit -qAm y
  $ hg up 'desc(x)'
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ echo z > z
  $ hg commit -qAm z
  $ cd ..

  $ initserver master masterrepo
  $ cd master
  $ hg log
  $ hg pull -q ../client

# Strip middle commit, verify sync fails

  $ hg debugstrip -r 'desc(y)' --config hgsql.bypass=True
- The huge number in the output below is because we're trying to apply rev 0
(which contains the generaldelta bit in the offset int) to a non-rev 0
location (so the generaldelta bit isn't stripped before the comparison)
  $ hg log -l 1 2>&1 | egrep 'Corruption.*offset'
  *CorruptionException: revision offset doesn't match prior length (12884967424 offset vs 3 length): data/z.i (glob)

# Recover middle commit, but on top, then try syncing (succeeds)

  $ hg unbundle -q $TESTTMP/master/.hg/strip-backup/d34c38483be9-3839604f-backup.hg --config hgsql.bypass=True
  $ hg log -l 1
  commit:      d34c38483be9
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  summary:     y
  
  $ cd ..

# Attempt to pull new commit should fail because base rev is wrong due to reordering

  $ cd client
  $ echo a > a
  $ hg commit -qAm a
  $ cd ../master
  $ hg pull ../client 2>&1 | egrep 'Corruption.*node'
  *CorruptionException: expected node d34c38483be9d08f205eaae60c380a29b48e0189 at rev 1 of 00changelog.i but found bc3a71defa4a8fb6e8e5c192c02a26d94853d281 (glob)

# Fix ordering, can pull now

  $ hg debugstrip -r 'desc(z)' --config hgsql.bypass=True
  $ hg unbundle -q $TESTTMP/master/.hg/strip-backup/bc3a71defa4a-48128f20-backup.hg --config hgsql.bypass=True
  $ hg pull ../client
  pulling from ../client
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
