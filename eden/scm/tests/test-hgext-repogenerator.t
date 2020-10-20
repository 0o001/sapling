#require py2
#chg-compatible

  $ newrepo
  $ setconfig repogenerator.filenamedircount=2
  $ setconfig repogenerator.filenameleaflength=1
  $ setconfig repogenerator.numcommits=3
  $ hg repogenerator --seed 1 --config extensions.repogenerator=
  starting commit is: -1 (goal is 2)
  created *, * sec elapsed (* commits/sec, * per hour, * per day) (glob)
  $ hg log -G -r ::tip
  o  commit:      26c418a67612
  │  user:        test
  │  date:        Thu Jan 01 00:00:00 1970 +0000
  │  summary:     memory commit
  │
  o  commit:      331925392347
  │  user:        test
  │  date:        Thu Jan 01 00:00:00 1970 +0000
  │  summary:     memory commit
  │
  o  commit:      af3f7799efa3
     user:        test
     date:        Thu Jan 01 00:00:00 1970 +0000
     summary:     memory commit
  
  $ hg up -C tip
  13 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg log -r tip -T'{files}'
  d/d/c f/n/p i/t/b n/y/p u/d/a y/n/h (no-eol)
  $ ls */*/*
  d/d/c
  f/n/p
  i/t/b
  j/i/e
  j/q/a
  k/s/d
  n/y/p
  t/a/e
  u/d/a
  u/r/g
  x/m/c
  x/n/c
  y/n/h

Set startcommit=0 and confirm it creates a commit off of 0.
  $ setconfig repogenerator.startcommit=0
  $ hg repogenerator --seed 1 --config extensions.repogenerator= -n 1
  starting commit is: 0 (goal is 2)
  created 0, * sec elapsed (* commits/sec, * per hour, * per day) (glob)
  generated 1 commits; quitting
  $ hg log -r tip~1+tip -T '{node} '
  af3f7799efa33044a6588f81254cb7c85d7d4de2 22df286c492c87bc6af7d82e95073bc51fb5631f  (no-eol)
