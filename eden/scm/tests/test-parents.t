#chg-compatible

test parents command

  $ hg init repo
  $ cd repo

no working directory

  $ hg parents

  $ echo a > a
  $ echo b > b
  $ hg ci -Amab -d '0 0'
  adding a
  adding b
  $ echo a >> a
  $ hg ci -Ama -d '1 0'
  $ echo b >> b
  $ hg ci -Amb -d '2 0'
  $ echo c > c
  $ hg ci -Amc -d '3 0'
  adding c
  $ hg up -C 1
  1 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ echo d > c
  $ hg ci -Amc2 -d '4 0'
  adding c
  $ hg up -C 3
  2 files updated, 0 files merged, 0 files removed, 0 files unresolved


  $ hg parents
  commit:      02d851b7e549
  user:        test
  date:        Thu Jan 01 00:00:03 1970 +0000
  summary:     c
  

  $ hg parents a
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  

hg parents c, single revision

  $ hg parents c
  commit:      02d851b7e549
  user:        test
  date:        Thu Jan 01 00:00:03 1970 +0000
  summary:     c
  

  $ hg parents -r 3 c
  abort: 'c' not found in manifest!
  [255]

  $ hg parents -r 2
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  

  $ hg parents -r 2 a
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  

  $ hg parents -r 2 ../a
  abort: ../a not under root '$TESTTMP/repo'
  [255]


cd dir; hg parents -r 2 ../a

  $ mkdir dir
  $ cd dir
  $ hg parents -r 2 ../a
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  
  $ hg parents -r 2 path:a
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  
  $ cd ..

  $ hg parents -r 2 glob:a
  commit:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:01 1970 +0000
  summary:     a
  


merge working dir with 2 parents, hg parents c

  $ HGMERGE=true hg merge
  merging c
  0 files updated, 1 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)
  $ hg parents c
  commit:      02d851b7e549
  user:        test
  date:        Thu Jan 01 00:00:03 1970 +0000
  summary:     c
  
  commit:      48cee28d4b4e
  parent:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:04 1970 +0000
  summary:     c2
  


merge working dir with 1 parent, hg parents

  $ hg up -C 2
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ HGMERGE=true hg merge -r 4
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)
  $ hg parents
  commit:      6cfac479f009
  user:        test
  date:        Thu Jan 01 00:00:02 1970 +0000
  summary:     b
  
  commit:      48cee28d4b4e
  parent:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:04 1970 +0000
  summary:     c2
  

merge working dir with 1 parent, hg parents c

  $ hg parents c
  commit:      48cee28d4b4e
  parent:      d786049f033a
  user:        test
  date:        Thu Jan 01 00:00:04 1970 +0000
  summary:     c2
  

  $ cd ..
