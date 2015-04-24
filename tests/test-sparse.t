test sparse

  $ hg init myrepo
  $ cd myrepo
  $ cat > .hg/hgrc <<EOF
  > [extensions]
  > sparse=$(dirname $TESTDIR)/sparse.py
  > strip=
  > EOF

  $ echo a > show
  $ echo x > hide
  $ hg ci -Aqm 'initial'

  $ echo b > show
  $ echo y > hide
  $ echo aa > show2
  $ echo xx > hide2
  $ hg ci -Aqm 'two'

Verify basic --include

  $ hg up -q 0
  $ hg sparse --include 'hide'
  $ ls
  hide

Verify commiting while sparse includes other files

  $ echo z > hide
  $ hg ci -Aqm 'edit hide'
  $ ls
  hide
  $ hg manifest
  hide
  show

Verify --reset brings files back

  $ hg sparse --reset
  $ ls
  hide
  show
  $ cat hide
  z
  $ cat show
  a

Verify 'hg sparse' default output

  $ hg up -q null
  $ hg sparse --include 'show*'

  $ hg sparse
  [include]
  show*
  [exclude]
  
  
Verify update only writes included files

  $ hg up -q 0
  $ ls
  show

  $ hg up -q 1
  $ ls
  show
  show2

Verify status only shows included files

  $ touch hide
  $ touch hide3
  $ echo c > show
  $ hg status
  M show

Adding an excluded file should fail

  $ hg add hide3
  abort: cannot add 'hide3' - it is outside the sparse checkout
  [255]

Verify deleting sparseness while a file has changes fails

  $ hg sparse --delete 'show*'
  pending changes to 'hide'
  abort: cannot change sparseness due to pending changes (delete the files or use --force to bring them back dirty)
  [255]

Verify deleting sparseness with --force brings back files

  $ hg sparse --delete -f 'show*'
  pending changes to 'hide'
  $ ls
  hide
  hide2
  hide3
  show
  show2
  $ hg st
  M hide
  M show
  ? hide3

Verify editting sparseness fails if pending changes

  $ hg sparse --include 'show*'
  pending changes to 'hide'
  abort: could not update sparseness due to pending changes
  [255]

Verify adding sparseness hides files

  $ hg sparse --exclude -f 'hide*'
  pending changes to 'hide'
  $ ls
  hide
  hide3
  show
  show2
  $ hg st
  M show

  $ hg up -qC .
  $ hg purge --all --config extensions.purge=
  $ ls
  show
  show2

Verify rebase temporarily includes excluded files

  $ hg rebase -d 1 -r 2 --config extensions.rebase=
  rebasing 2:b91df4f39e75 "edit hide" (tip)
  temporarily included 1 file(s) in the sparse checkout for merging
  merging hide
  warning: conflicts during merge.
  merging hide incomplete! (edit conflicts, then use 'hg resolve --mark')
  unresolved conflicts (see hg resolve, then hg rebase --continue)
  [1]

  $ hg sparse
  [include]
  
  [exclude]
  hide*
  
  Temporarily Included Files (for merge/rebase):
  hide

  $ cat hide
  <<<<<<< dest:   39278f7c08a9  - test: two
  y
  =======
  z
  >>>>>>> source: b91df4f39e75 - test: edit hide

Verify aborting a rebase cleans up temporary files

  $ hg rebase --abort --config extensions.rebase=
  cleaned up 1 temporarily added file(s) from the sparse checkout
  rebase aborted
  $ rm hide.orig

  $ ls
  show
  show2

Verify merge fails if merging excluded files

  $ hg up -q 1
  $ hg merge -r 2
  temporarily included 1 file(s) in the sparse checkout for merging
  merging hide
  warning: conflicts during merge.
  merging hide incomplete! (edit conflicts, then use 'hg resolve --mark')
  0 files updated, 0 files merged, 0 files removed, 1 files unresolved
  use 'hg resolve' to retry unresolved file merges or 'hg update -C .' to abandon
  [1]
  $ hg sparse
  [include]
  
  [exclude]
  hide*
  
  Temporarily Included Files (for merge/rebase):
  hide

  $ hg up -C .
  cleaned up 1 temporarily added file(s) from the sparse checkout
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg sparse
  [include]
  
  [exclude]
  hide*
  

Verify strip -k resets dirstate correctly

  $ hg status
  $ hg sparse
  [include]
  
  [exclude]
  hide*
  
  $ hg log -r . -T '{rev}\n' --stat
  1
   hide  |  2 +-
   hide2 |  1 +
   show  |  2 +-
   show2 |  1 +
   4 files changed, 4 insertions(+), 2 deletions(-)
  
  $ hg strip -r . -k
  saved backup bundle to $TESTTMP/myrepo/.hg/strip-backup/39278f7c08a9-ce59e002-backup.hg (glob)
  $ hg status
  M show
  ? show2

Verify rebase succeeds if all changed files are in sparse checkout

  $ hg commit -Aqm "add show2"
  $ hg rebase -d 1 --config extensions.rebase=
  rebasing 2:bdde55290160 "add show2" (tip)
  saved backup bundle to $TESTTMP/myrepo/.hg/strip-backup/bdde55290160-216ed9c6-backup.hg (glob)

Verify log --sparse only shows commits that affect the sparse checkout

  $ hg log -T '{rev} '
  2 1 0  (no-eol)
  $ hg log --sparse -T '{rev} '
  2 0  (no-eol)

Test status on a file in a subdir

  $ mkdir -p dir1/dir2
  $ touch dir1/dir2/file
  $ hg sparse -I dir1/dir2
  $ hg status
  ? dir1/dir2/file

Test hgwatchman integration (if available)

  $ $PYTHON -c 'import hgwatchman' || exit 80
  $ echo "ignoredir1/" >> .hgignore
  $ hg add .hgignore
  $ hg commit -m ignoredir1
  $ echo "ignoredir2/" >> .hgignore
  $ hg commit -m ignoredir2

  $ hg sparse -I ignoredir1 -I ignoredir2

  $ mkdir ignoredir1 ignoredir2
  $ touch ignoredir1/file ignoredir2/file

Run status twice to compensate for a condition in hgwatchman where it will check
ignored files the second time it runs, regardless of previous state (ask @sid0)
  $ hg status --config extensions.hgwatchman=
  ? dir1/dir2/file
  $ hg status --config extensions.hgwatchman=
  ? dir1/dir2/file

Test that hgwatchmans ignore hash check updates when .hgignore changes

  $ hg up -q .^
  $ hg status --config extensions.hgwatchman=
  ? dir1/dir2/file
  ? ignoredir2/file
