Load commonly used test logic
  $ . "$TESTDIR/hggit/testutil"

  $ git init -q gitrepo
  $ cd gitrepo
  $ echo alpha > alpha
  $ git add alpha
  $ fn_git_commit -m 'add alpha'

  $ git checkout -b beta
  Switched to a new branch 'beta'
  $ echo beta > beta
  $ git add beta
  $ fn_git_commit -m 'add beta'


  $ cd ..

clone a branch
  $ hg clone -r beta gitrepo hgrepo-b 2>&1 | grep -v '^updating'
  importing git objects into hg
  2 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg -R hgrepo-b log --graph
  @  commit:      3bb02b6794dd
  │  bookmark:    beta
  │  user:        test <test@example.org>
  │  date:        Mon Jan 01 00:00:11 2007 +0000
  │  summary:     add beta
  │
  o  commit:      69982ec78c6d
     bookmark:    master
     user:        test <test@example.org>
     date:        Mon Jan 01 00:00:10 2007 +0000
     summary:     add alpha
  
Make sure this is still draft since we didn't pull remote's HEAD
  $ hg -R hgrepo-b phase -r beta
  3bb02b6794ddc0b498cdc15f59f2e6724cabfa2f: draft

clone with mapsavefreq set
  $ rm -rf hgrepo-b
  $ hg clone -r beta gitrepo hgrepo-b --config hggit.mapsavefrequency=1 --debug 2>&1 | egrep "(saving|committing)"
  committing files:
  committing manifest
  committing changelog
  committing transaction
  saving mapfile
  committing files:
  committing manifest
  committing changelog
  committing transaction
  saving mapfile

Make sure that a deleted .hgsubstate does not confuse hg-git

  $ cd gitrepo
  $ echo 'HASH random' > .hgsubstate
  $ git add .hgsubstate
  $ fn_git_commit -m 'add bogus .hgsubstate'
  $ git rm -q .hgsubstate
  $ fn_git_commit -m 'remove bogus .hgsubstate'
  $ cd ..

  $ hg clone -r beta gitrepo hgrepo-c
  importing git objects into hg
  updating to branch default
  2 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg --cwd hgrepo-c status

clone empty repo
  $ git init -q empty
  $ hg clone empty emptyhg
  updating to branch default
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved

Ensure hggit.disallowinitbare blocks initting .hg/git
  $ hg init nogitbare
  $ cd nogitbare
  $ cat >> .hg/hgrc <<EOF
  > [hggit]
  > disallowinitbare=True
  > EOF
  $ hg pull ../empty
  pulling from ../empty
  abort: missing .hg/git repo
  [255]
