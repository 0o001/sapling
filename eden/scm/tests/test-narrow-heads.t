#chg-compatible

'narrow-heads' requires remotenames and visibility

  $ enable remotenames amend
  $ setconfig experimental.narrow-heads=true visibility.enabled=true mutation.record=true mutation.enabled=true mutation.date="0 0" experimental.evolution= remotenames.rename.default=remote
  $ setconfig 'infinitepush.branchpattern=re:(^hack/.*)'
  $ shorttraceback

Prepare the server repo

  $ newrepo server
  $ setconfig treemanifest.server=true
  $ drawdag << 'EOS'
  > B
  > |
  > A
  > EOS

  $ hg bookmark -r $B master
  $ hg bookmark -r $A stable

Prepare the client repo
('--pull' during clone is required to get visibility requirement set)

  $ hg clone $TESTTMP/server $TESTTMP/client -q --pull
  $ cd $TESTTMP/client

Verify the commits

  $ hg bookmarks --remote
     remote/master             1:112478962961
     remote/stable             0:426bada5c675

Revsets after the initial clone

  $ hg log -Gr 'all()' -T '{desc} {remotenames} {phase}'
  @  B remote/master public
  |
  o  A remote/stable public
  
  $ hg log -Gr 'head()' -T '{desc} {remotenames}'
  @  B remote/master
  |
  ~

Make some client-side commits based on A

  $ drawdag << 'EOS'
  > D
  > |
  > C
  > |
  > A
  > EOS
  $ hg up -r $D -q
  $ hg up -r $C -q
  $ hg metaedit -m C2

Revsets after the local edits

head() should include one 'D' commit, and one 'B'

  $ hg log -Gr 'head()' -T '{desc}'
  o  D
  |
  ~
  o  B
  |
  ~

all() should not show C
Commits under ::master should be public

  $ hg log -Gr 'all()' -T '{desc} {phase} {remotebookmarks}'
  o  D draft
  |
  @  C2 draft
  |
  | o  B public remote/master
  |/
  o  A public remote/stable
  
draft() should not show C

  $ hg log -Gr 'draft()' -T '{desc}'
  o  D
  |
  @  C2
  |
  ~
not public() should not show C

  $ hg log -Gr 'not public()' -T '{desc}'
  o  D
  |
  @  C2
  |
  ~
A:: should not show C

  $ hg log -Gr "$A::" -T '{desc}'
  o  D
  |
  @  C2
  |
  | o  B
  |/
  o  A
  
children(A) should not show C

  $ hg log -Gr "children($A)" -T '{desc}'
  @  C2
  |
  ~
  o  B
  |
  ~

predecessors(C2) should include C

  $ hg log -Gr "predecessors(desc('C2'))" -T '{desc}'
  @  C2
  |
  ~
  x  C
  |
  ~

Using commit hash to access C should be allowed

  $ hg log -r $C -T '{desc}'
  C (no-eol)

Phases

  $ hg phase --public $D
  (phases are now managed by remotenames and heads; manully editing phases is a no-op)
  $ hg phase $D
  e7b3f00ed42ef8977173765eccff8a861809549b: secret

  $ hg phase --force --draft $A
  (phases are now managed by remotenames and heads; manully editing phases is a no-op)
  $ hg phase $A
  426bada5c67598ca65036d57d9e4b64b0c1ce7a0: public

Rebase

  $ newrepo
  $ enable rebase amend
  $ drawdag << 'EOS'
  > B C
  > |/
  > | D
  > |/
  > A
  > EOS
  $ hg debugremotebookmark master $B
  $ hg hide $D -q
  $ hg rebase -s $D -d $B
  "source" revision set is invisible - nothing to rebase
  (hint: use 'hg unhide' to make commits visible first)

Visible heads got out of sync with "." or bookmarks

  $ newrepo
  $ drawdag << 'EOS'
  > M B
  > |/
  > | C
  > |/
  > | D
  > |/
  > A
  > EOS
  $ hg debugremotebookmark master $M
  $ hg hide -q $B+$C+$D
  $ hg up -q $C
  $ hg bookmark -r $D book-D

 (Both C and D should show up since they are working parents and bookmarked)
  $ hg log -Gr 'all()' -T '{desc} {phase}'
  o  M public
  |
  | o  D draft
  |/
  | @  C draft
  |/
  o  A public
  
 (Both C and D should show up here, too)
  $ hg log -Gr 'draft()' -T '{desc} {phase}'
  o  D draft
  |
  ~
  @  C draft
  |
  ~


Prepare the server repo with draft branches

  $ newrepo server1
  $ setconfig treemanifest.server=true
  $ drawdag << 'EOS'
  > B C
  > |/
  > A
  > EOS

  $ hg bookmark -r $B master
  $ hg bookmark -r $C hack/feature-foo

Prepare the client repo

  $ hg clone $TESTTMP/server1 $TESTTMP/client1 -q --pull -U
  $ cd $TESTTMP/client1

  $ hg log -r 'draft()' -T '{desc}\n'
  C

Check the draft branch can be hidden

  $ hg hide $C
  hiding commit dc0947a82db8 "C"
  1 changeset hidden

  $ hg log -r 'draft()' -T '{desc}\n'
