  $ newrepo
  $ mkdir -p dirA/subdirA dirA/subdirB dirB
  $ touch dirA/subdirA/file1 dirA/subdirB/file2 dirB/file3 file4
  $ hg commit -Aqm "base"

Check basic case collisions
  $ hg debugcheckcasecollisions DIRA/subdira/FILE1 DIRA/SUBDIRB/file2 DIRB/FILE3
  dirA/subdirA/file1 conflicts with DIRA/subdira/FILE1
  dirA/subdirA (directory for dirA/subdirA/file1) conflicts with DIRA/subdira (directory for DIRA/subdira/FILE1)
  dirA (directory for dirA/subdirA/file1) conflicts with DIRA (directory for DIRA/SUBDIRB/file2)
  dirA/subdirB/file2 conflicts with DIRA/SUBDIRB/file2
  dirA/subdirB (directory for dirA/subdirB/file2) conflicts with DIRA/SUBDIRB (directory for DIRA/SUBDIRB/file2)
  dirB/file3 conflicts with DIRB/FILE3
  dirB (directory for dirB/file3) conflicts with DIRB (directory for DIRB/FILE3)
  [1]

Check a dir that collides with a file
  $ hg debugcheckcasecollisions FILE4/foo
  file4 conflicts with FILE4 (directory for FILE4/foo)
  [1]

Check a file that collides with a dir
  $ hg debugcheckcasecollisions DIRb
  dirB (directory for dirB/file3) conflicts with DIRb
  [1]

Check self-conflicts
  $ hg debugcheckcasecollisions newdir/newfile NEWdir/newfile newdir/NEWFILE
  NEWdir/newfile conflicts with newdir/newfile
  NEWdir (directory for NEWdir/newfile) conflicts with newdir (directory for newdir/newfile)
  newdir/NEWFILE conflicts with newdir/newfile
  [1]

Check against a particular revision
  $ hg debugcheckcasecollisions -r 0 FILE4
  file4 conflicts with FILE4
  [1]
