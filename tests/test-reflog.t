  $ extpath=$(dirname $TESTDIR)
  $ cp $extpath/reflog.py $TESTTMP # use $TESTTMP substitution in message
  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > reflog=$TESTTMP/reflog.py
  > EOF

  $ hg init repo
  $ cd repo

Test empty reflog

  $ hg reflog
  Previous locations of '.':
  no recorded locations
  $ hg reflog fakebookmark
  Previous locations of 'fakebookmark':
  no recorded locations

Test that working copy changes are tracked

  $ echo a > a
  $ hg commit -Aqm a
  $ hg reflog
  Previous locations of '.':
  cb9a9f314b8b  commit -Aqm a
  $ echo b > a
  $ hg commit -Aqm b
  $ hg reflog
  Previous locations of '.':
  1e6c11564562  commit -Aqm b
  cb9a9f314b8b  commit -Aqm a
  $ hg up 0
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg reflog
  Previous locations of '.':
  cb9a9f314b8b  up 0
  1e6c11564562  commit -Aqm b
  cb9a9f314b8b  commit -Aqm a

Test that bookmarks are tracked

  $ hg book -r tip foo
  $ hg reflog foo
  Previous locations of 'foo':
  1e6c11564562  book -r tip foo
  $ hg book  -f foo
  $ hg reflog foo
  Previous locations of 'foo':
  cb9a9f314b8b  book -f foo
  1e6c11564562  book -r tip foo
  $ hg up
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  updating bookmark foo
  $ hg reflog foo
  Previous locations of 'foo':
  1e6c11564562  up
  cb9a9f314b8b  book -f foo
  1e6c11564562  book -r tip foo

Test that bookmarks and working copy tracking is not mixed

  $ hg reflog
  Previous locations of '.':
  1e6c11564562  up
  cb9a9f314b8b  up 0
  1e6c11564562  commit -Aqm b
  cb9a9f314b8b  commit -Aqm a

Test verbose output

  $ hg reflog -v
  Previous locations of '.':
  cb9a9f314b8b -> 1e6c11564562 * *  up (glob)
  1e6c11564562 -> cb9a9f314b8b * *  up 0 (glob)
  cb9a9f314b8b -> 1e6c11564562 * *  commit -Aqm b (glob)
  000000000000 -> cb9a9f314b8b * *  commit -Aqm a (glob)

  $ hg reflog -v foo
  Previous locations of 'foo':
  cb9a9f314b8b -> 1e6c11564562 * *  up (glob)
  1e6c11564562 -> cb9a9f314b8b * *  book -f foo (glob)
  000000000000 -> 1e6c11564562 * *  book -r tip foo (glob)

Test JSON output

  $ hg reflog -T json
  [
   {
    "command": "up",
    "date": "*", (glob)
    "newhashes": "1e6c11564562",
    "oldhashes": "cb9a9f314b8b",
    "user": "*" (glob)
   },
   {
    "command": "up 0",
    "date": "*", (glob)
    "newhashes": "cb9a9f314b8b",
    "oldhashes": "1e6c11564562",
    "user": "*" (glob)
   },
   {
    "command": "commit -Aqm b",
    "date": "*", (glob)
    "newhashes": "1e6c11564562",
    "oldhashes": "cb9a9f314b8b",
    "user": "*" (glob)
   },
   {
    "command": "commit -Aqm a",
    "date": "*", (glob)
    "newhashes": "cb9a9f314b8b",
    "oldhashes": "000000000000",
    "user": "*" (glob)
   }
  ]
