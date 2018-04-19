#require p4

  $ . $TESTDIR/p4setup.sh
  $ cat >> $HGRCPATH<<EOF
  > [extensions]
  > lfs=
  > [p4fastimport]
  > lfspointeronly=True
  > lfsmetadata=lfs.sql
  > [lfs]
  > threshold=10
  > EOF

  $ p4 client -o hg-p4-import-narrow | sed '/^View:/,$ d' >p4client
  $ echo "View:" >>p4client
  $ echo " //depot/Main/... //hg-p4-import-narrow/Main/..." >>p4client
  $ p4 client -i <p4client
  Client hg-p4-import-narrow saved.

populate the depot
  $ mkdir Main
  $ echo a > Main/a
  $ echo thisisasuperlargefilebewithmorethank10ksize >> Main/largefile
  $ p4 add Main/a  Main/largefile
  //depot/Main/a#1 - opened for add
  //depot/Main/largefile#1 - opened for add
  $ p4 submit -d initial
  Submitting change 1.
  Locking 2 files ...
  add //depot/Main/a#1
  add //depot/Main/largefile#1
  Change 1 submitted.

  $ p4 edit Main/a  Main/largefile
  //depot/Main/a#1 - opened for edit
  //depot/Main/largefile#1 - opened for edit
  $ echo a >> Main/a
  $ echo thisisasuperlargefilebewithmorethank10ksize >> Main/largefile
  $ p4 submit -d second
  Submitting change 2.
  Locking 2 files ...
  edit //depot/Main/a#2
  edit //depot/Main/largefile#2
  Change 2 submitted.

Sync Commit

  $ cd $hgwd
  $ hg init --config 'format.usefncache=False'
  $ hg p4seqimport --debug -P $P4ROOT hg-p4-import-narrow
  loading changelist numbers.
  2 changelists to import.
  importing CL1
  committing files:
  Main/a
  file: //depot/Main/a, src: * (glob)
  Main/largefile
  file: //depot/Main/largefile, src: * (glob)
  committing manifest
  committing changelog
  largefile: Main/largefile, oid: 37a7b43abd9e105a0e6b22088b140735a02f288767fe7a6f4f436cb46b064ca9
  writing lfs metadata to sqlite
  importing CL2
  file: //depot/Main/a, src: rcs
  file: //depot/Main/largefile, src: rcs
  committing files:
  Main/a
  Main/largefile
  committing manifest
  committing changelog
  largefile: Main/largefile, oid: b0d5c1968efbabbff9d94160f284cd7b52686ca3c46cfffdd351de07384fce9c
  writing lfs metadata to sqlite
  updating the branch cache


  $ cd $p4wd
  $ mkdir Outside
  $ echo thisisanotherlargefile > Outside/anotherlargefile
  $ p4 add Outside/anotherlargefile
  //depot/Outside/anotherlargefile#1 - opened for add
  $ p4 submit -d third
  Submitting change 3.
  Locking 1 files ...
  add //depot/Outside/anotherlargefile#1
  Change 3 submitted.

  $ cd $hgwd
  $ hg p4syncimport --debug -P $P4ROOT hg-p4-import-narrow hg-p4-import
  incremental import from changelist: 3, node: * (glob)
  2 (current client) 3 (requested client) 2 (latest imported)
  latest change list number 3
  3 p4 filelogs to read
  1 new filelogs and 2 reuse filelogs
  running a sync import.
  writing filelog: cf38a89d2b54, p1 000000000000, linkrev 2, 23 bytes, src: *, path: Outside/anotherlargefile (glob)
  largefile: Outside/anotherlargefile, oid: 9703972eff7a4df07317eda436ab7ef827ed16ea28c62abdcd7de269745c610c
  changelist 3: writing manifest. node: ff600511f8ae p1: c14352bb3510 p2: 000000000000 linkrev: 2
  changelist 3: writing changelog: p4fastimport synchronizing client view
  writing lfs metadata to sqlite
  updating the branch cache
  1 revision, 1 file(s) imported.
  $ hg manifest -r tip
  Main/a
  Main/largefile
  Outside/anotherlargefile

Verify
(waiting for https://patchwork.mercurial-scm.org/patch/20582/)

  $ cd $hgwd
  $ hg --debug verify --config verify.skipflags=8192
  repository uses revlog format 1
  checking changesets
  checking manifests
  crosschecking files in changesets and manifests
  checking files
  3 files, 3 changesets, 5 total revisions

  $ test -d .hg/store/lfs/objects
  [1]
  $ sqlite3 lfs.sql "SELECT * FROM p4_lfs_map"
  1|1|*|37a7b43abd9e105a0e6b22088b140735a02f288767fe7a6f4f436cb46b064ca9|//depot/Main/largefile (glob)
  2|2|*|b0d5c1968efbabbff9d94160f284cd7b52686ca3c46cfffdd351de07384fce9c|//depot/Main/largefile (glob)
  3|3|*|9703972eff7a4df07317eda436ab7ef827ed16ea28c62abdcd7de269745c610c|//depot/Outside/anotherlargefile (glob)

End Test

  stopping the p4 server
