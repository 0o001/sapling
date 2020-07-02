#chg-compatible

  $ . "$TESTDIR/histedit-helpers.sh"

  $ enable histedit

Create repo a:

  $ hg init a
  $ cd a
  $ hg unbundle "$TESTDIR/bundles/rebase.hg"
  adding changesets
  adding manifests
  adding file changes
  added 8 changesets with 7 changes to 7 files
  $ hg up tip
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved

  $ tglogp
  @  7: 02de42196ebe draft 'H'
  |
  | o  6: eea13746799a draft 'G'
  |/|
  o |  5: 24b6387c8c8c draft 'F'
  | |
  | o  4: 9520eea781bc draft 'E'
  |/
  | o  3: 32af7686d403 draft 'D'
  | |
  | o  2: 5fddd98957c8 draft 'C'
  | |
  | o  1: 42ccdea3bb16 draft 'B'
  |/
  o  0: cd010b8cd998 draft 'A'
  
Verify that implicit base command and help are listed

  $ HGEDITOR=cat hg histedit |grep base
  #  b, base = checkout changeset and apply further changesets from there

Go to D
  $ hg update 'desc(D)'
  3 files updated, 0 files merged, 2 files removed, 0 files unresolved
edit the history to rebase B onto H


Rebase B onto H
  $ hg histedit 'max(desc(B))' --commands - 2>&1 << EOF | fixbundle
  > base 02de42196ebe
  > pick 42ccdea3bb16 B
  > pick 5fddd98957c8 C
  > pick 32af7686d403 D
  > EOF

  $ tglogp
  @  10: 0937e82309df draft 'D'
  |
  o  9: f778d1cbddac draft 'C'
  |
  o  8: 3d41b7cc7085 draft 'B'
  |
  o  7: 02de42196ebe draft 'H'
  |
  | o  6: eea13746799a draft 'G'
  |/|
  o |  5: 24b6387c8c8c draft 'F'
  | |
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
Rebase back and drop something
  $ hg histedit 'max(desc(B))' --commands - 2>&1 << EOF | fixbundle
  > base cd010b8cd998
  > pick 3d41b7cc7085 B
  > drop f778d1cbddac C
  > pick 0937e82309df D
  > EOF

  $ tglogp
  @  12: 476cc3e4168d draft 'D'
  |
  o  11: d273e35dcdf2 draft 'B'
  |
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
Split stack
  $ hg histedit 'max(desc(B))' --commands - 2>&1 << EOF | fixbundle
  > base cd010b8cd998
  > pick d273e35dcdf2 B
  > base cd010b8cd998
  > pick 476cc3e4168d D
  > EOF

  $ tglogp
  @  13: d7a6f907a822 draft 'D'
  |
  | o  11: d273e35dcdf2 draft 'B'
  |/
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
Abort
  $ echo x > B
  $ hg add B
  $ hg commit -m "X"
  $ tglogp
  @  14: 591369deedfd draft 'X'
  |
  o  13: d7a6f907a822 draft 'D'
  |
  | o  11: d273e35dcdf2 draft 'B'
  |/
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
  $ hg histedit 'max(desc(D))' --commands - 2>&1 << EOF | fixbundle
  > base d273e35dcdf2 B
  > drop d7a6f907a822 D
  > pick 591369deedfd X
  > EOF
  merging B
  warning: 1 conflicts while merging B! (edit, then use 'hg resolve --mark')
  Fix up the change (pick 591369deedfd)
  (hg histedit --continue to resume)
  $ hg histedit --abort | fixbundle
  2 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ tglogp
  @  14: 591369deedfd draft 'X'
  |
  o  13: d7a6f907a822 draft 'D'
  |
  | o  11: d273e35dcdf2 draft 'B'
  |/
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
Continue
  $ hg histedit 'max(desc(D))' --commands - 2>&1 << EOF | fixbundle
  > base d273e35dcdf2 B
  > drop d7a6f907a822 D
  > pick 591369deedfd X
  > EOF
  merging B
  warning: 1 conflicts while merging B! (edit, then use 'hg resolve --mark')
  Fix up the change (pick 591369deedfd)
  (hg histedit --continue to resume)
  $ echo b2 > B
  $ hg resolve --mark B
  (no more unresolved files)
  continue: hg histedit --continue
  $ hg histedit --continue | fixbundle
  $ tglogp
  @  15: 03772da75548 draft 'X'
  |
  o  11: d273e35dcdf2 draft 'B'
  |
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  

base on a previously picked changeset
  $ echo i > i
  $ hg add i
  $ hg commit -m "I"
  $ echo j > j
  $ hg add j
  $ hg commit -m "J"
  $ tglogp
  @  17: e8c55b19d366 draft 'J'
  |
  o  16: b2f90fd8aa85 draft 'I'
  |
  o  15: 03772da75548 draft 'X'
  |
  o  11: d273e35dcdf2 draft 'B'
  |
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
  $ hg histedit 'max(desc(B))' --commands - 2>&1 << EOF | fixbundle
  > pick d273e35dcdf2 B
  > pick 03772da75548 X
  > base d273e35dcdf2 B
  > pick e8c55b19d366 J
  > base d273e35dcdf2 B
  > pick b2f90fd8aa85 I
  > EOF
  hg: parse error: base "d273e35dcdf2" changeset was an edited list candidate
  (base must only use unlisted changesets)

  $ tglogp
  @  17: e8c55b19d366 draft 'J'
  |
  o  16: b2f90fd8aa85 draft 'I'
  |
  o  15: 03772da75548 draft 'X'
  |
  o  11: d273e35dcdf2 draft 'B'
  |
  | o  7: 02de42196ebe draft 'H'
  | |
  | | o  6: eea13746799a draft 'G'
  | |/|
  | o |  5: 24b6387c8c8c draft 'F'
  |/ /
  | o  4: 9520eea781bc draft 'E'
  |/
  o  0: cd010b8cd998 draft 'A'
  
