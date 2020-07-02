#chg-compatible

  $ configure mutation-norecord
#require rmcwd

Ensure that dirsync does not cause an abort when cwd goes missing

  $ enable rebase dirsync
  $ setconfig phases.publish=False

  $ newrepo
  $ drawdag <<'EOF'
  >   change    # change/a = a
  >    |
  >    | delete # delete/dir/a = (removed)
  >    | /
  >   base      # base/dir/a = a
  > EOF

  $ hg co -q $change
  $ cd dir

  $ hg rebase -s . -d $delete
  rebasing 6f52fdb3a577 "change"
  current directory was removed
  (consider changing to repo root: $TESTTMP/repo1)

  $ cd -
  $TESTTMP/repo1
  $ hg status

  $ hg log -Gr "all()" -T "{node|short} {desc}"
  @  74e7da63e173 change
  |
  o  5f45087392e8 delete
  |
  o  aa6caddcd04f base
  
  $ hg rebase --abort
  abort: no rebase in progress
  [255]
