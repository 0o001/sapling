#chg-compatible

  $ configure mutation-norecord
#require execbit

  $ tellmeabout() {
  > if [ -x $1 ]; then
  >     echo $1 is an executable file with content:
  >     cat $1
  > else
  >     echo $1 is a plain file with content:
  >     cat $1
  > fi
  > }

  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > rebase=
  > EOF

Test rebasing a single commit that changes flags:
  $ hg init repo
  $ cd repo
  $ echo "A" > foo
  $ hg add foo
  $ chmod +x foo
  $ tellmeabout foo
  foo is an executable file with content:
  A
  $ hg com -m "base"
  $ hg mv foo foo_newloc
  $ hg com -m "move"
  $ hg up .~1
  1 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ echo "B" > foo
  $ hg com -m "change"
  $ hg log -G
  @  changeset:   2:a7f7eece6b0c
  |  parent:      0:c0233516197f
  |  user:        test
  |  date:        Thu Jan 01 00:00:00 1970 +0000
  |  summary:     change
  |
  | o  changeset:   1:5f41048406b0
  |/   user:        test
  |    date:        Thu Jan 01 00:00:00 1970 +0000
  |    summary:     move
  |
  o  changeset:   0:c0233516197f
     user:        test
     date:        Thu Jan 01 00:00:00 1970 +0000
     summary:     base
  

  $ hg rebase -r 1 -d .
  rebasing 5f41048406b0 "move"
  merging foo and foo_newloc to foo_newloc
  $ hg up tip
  1 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ tellmeabout foo_newloc
  foo_newloc is an executable file with content:
  B
