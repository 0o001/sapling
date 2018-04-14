#require p4

  $ . $TESTDIR/p4setup.sh

populate the depot
  $ mkdir Main
  $ mkdir Main/b
  $ echo a > Main/a
  $ echo c > Main/b/c
  $ echo d > Main/d
  $ p4 add Main/a Main/b/c Main/d
  //depot/Main/a#1 - opened for add
  //depot/Main/b/c#1 - opened for add
  //depot/Main/d#1 - opened for add
  $ p4 submit -d initial
  Submitting change 1.
  Locking 3 files ...
  add //depot/Main/a#1
  add //depot/Main/b/c#1
  add //depot/Main/d#1
  Change 1 submitted.

  $ p4 delete Main/a
  //depot/Main/a#1 - opened for delete
  $ p4 submit -d second
  Submitting change 2.
  Locking 1 files ...
  delete //depot/Main/a#2
  Change 2 submitted.

  $ echo  a > Main/a
  $ p4 add Main/a
  //depot/Main/a#2 - opened for add
  $ p4 submit -d third
  Submitting change 3.
  Locking 1 files ...
  add //depot/Main/a#3
  Change 3 submitted.

Simple import

  $ cd $hgwd
  $ hg init --config 'format.usefncache=False'
  $ hg p4fastimport --debug -P $P4ROOT hg-p4-import
  loading changelist numbers.
  3 changelists to import.
  loading list of files.
  3 files to import.
  reading filelog * (glob)
  reading filelog * (glob)
  reading filelog * (glob)
  importing repository.
  writing filelog: b789fdd96dc2, p1 000000000000, linkrev 0, 2 bytes, src: *, path: Main/a (glob)
  writing filelog: b789fdd96dc2, p1 000000000000, linkrev 2, 2 bytes, src: *, path: Main/a (glob)
  writing filelog: 149da44f2a4e, p1 000000000000, linkrev 0, 2 bytes, src: *, path: Main/b/c (glob)
  writing filelog: a9092a3d84a3, p1 000000000000, linkrev 0, 2 bytes, src: *, path: Main/d (glob)
  changelist 1: writing manifest. node: * p1: 000000000000 p2: 000000000000 linkrev: 0 (glob)
  changelist 1: writing changelog: initial
  changelist 2: writing manifest. node: * p1: * p2: 000000000000 linkrev: 1 (glob)
  updating the branch cache (?)
  changelist 2: writing changelog: second
  changelist 3: writing manifest. node: * p1: * p2: 000000000000 linkrev: 2 (glob)
  changelist 3: writing changelog: third
  updating the branch cache
  3 revision(s), 3 file(s) imported.

Verify

  $ hg verify
  checking changesets
  checking manifests
  crosschecking files in changesets and manifests
  checking files
  3 files, 3 changesets, 3 total revisions

  $ hg update tip
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved

Check hg debug data
  $ hg debugdata -m 0
  Main/a\x00b789fdd96dc2f3bd229c1dd8eedf0fc60e2b68e3 (esc)
  Main/b/c\x00149da44f2a4e14f488b7bd4157945a9837408c00 (esc)
  Main/d\x00a9092a3d84a37b9993b5c73576f6de29b7ea50f6 (esc)
  $ hg debugdata -m 1
  Main/b/c\x00149da44f2a4e14f488b7bd4157945a9837408c00 (esc)
  Main/d\x00a9092a3d84a37b9993b5c73576f6de29b7ea50f6 (esc)
  $ hg debugdata -m 2
  Main/a\x00b789fdd96dc2f3bd229c1dd8eedf0fc60e2b68e3 (esc)
  Main/b/c\x00149da44f2a4e14f488b7bd4157945a9837408c00 (esc)
  Main/d\x00a9092a3d84a37b9993b5c73576f6de29b7ea50f6 (esc)
  $ hg debugindex Main/a
     rev    offset  length  delta linkrev nodeid       p1           p2
       0         0       3     -1       0 b789fdd96dc2 000000000000 000000000000

End Test

  stopping the p4 server
