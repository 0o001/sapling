Test functionality is present

  $ extpath=$(dirname $TESTDIR)
  $ cp $extpath/fbamend.py $TESTTMP # use $TESTTMP substitution in message
  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > fbamend=$TESTTMP/fbamend.py
  > EOF

  $ hg help commit | grep -- --fixup
      --fixup               (with --amend) rebase children commits from a
  $ hg help commit | grep -- --rebase
      --rebase              (with --amend) rebases children commits after the
  $ hg help amend
  hg amend [OPTION]...
  
  amend the current commit with more changes
  
  options ([+] can be repeated):
  
   -A --addremove           mark new/missing files as added/removed before
                            committing
   -e --edit                prompt to edit the commit message
      --rebase              rebases children commits after the amend
      --fixup               rebase children commits from a previous amend
   -I --include PATTERN [+] include names matching the given patterns
   -X --exclude PATTERN [+] exclude names matching the given patterns
   -m --message TEXT        use text as commit message
   -l --logfile FILE        read commit message from file
  
  (some details hidden, use --verbose to show complete help)

Test basic functions

  $ hg init repo
  $ cd repo
  $ echo a > a
  $ hg add a
  $ hg commit -m 'a'
  $ echo a >> a
  $ hg commit -m 'aa'
  $ echo b >> b
  $ hg add b
  $ hg commit -m 'b'
  $ hg up .^
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  $ echo a >> a
  $ hg amend
  
      +----------------------------------------+
      | Please read the Dex article on stacked |
      | diff workflows to understand how the   |
      | fbamend extension works:               |
      |                                        |
      |      https://fburl.com/hgstacks        |
      +----------------------------------------+
  
  warning: the commit's children were left behind
  (use 'hg amend --fixup' to rebase them)
  $ hg amend --fixup
  rebasing the children of 34414ab6546d.preamend
  rebasing 2:a764265b74cf "b"
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/a764265b74cf-c5eef4f8-backup.hg (glob)
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/86cf3bb05fcf-36a6cbd7-preamend-backup.hg (glob)
  $ echo a >> a
  $ hg amend --rebase
  rebasing the children of 7817096bf624.preamend
  rebasing 2:e1c831172263 "b"
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/e1c831172263-eee3b8f6-backup.hg (glob)
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/34414ab6546d-72d06a8e-preamend-backup.hg (glob)

Test that current bookmark is maintained

  $ hg bookmark bm
  $ hg bookmarks
   * bm                        1:7817096bf624
  $ echo a >> a
  $ hg amend --rebase
  rebasing the children of bm.preamend
  rebasing 2:1e390e3ec656 "b"
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/1e390e3ec656-8362bab7-backup.hg (glob)
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/7817096bf624-d72fddeb-preamend-backup.hg (glob)
  $ hg bookmarks
   * bm                        1:7635008c16e1

Test that bookmarked re-amends work well

  $ echo a >> a
  $ hg amend
  
      +----------------------------------------+
      | Please read the Dex article on stacked |
      | diff workflows to understand how the   |
      | fbamend extension works:               |
      |                                        |
      |      https://fburl.com/hgstacks        |
      +----------------------------------------+
  
  warning: the commit's children were left behind
  (use 'hg amend --fixup' to rebase them)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  edf5fd2f5332 aa bm
  |
  | o  2d6884e15790 b
  | |
  | o  7635008c16e1 aa bm.preamend
  |/
  o  cb9a9f314b8b a
  
  $ echo a >> a
  $ hg amend
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/edf5fd2f5332-81b0ec5b-amend-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  0889a0030a17 aa bm
  |
  | o  2d6884e15790 b
  | |
  | o  7635008c16e1 aa bm.preamend
  |/
  o  cb9a9f314b8b a
  
  $ hg amend --fixup
  rebasing the children of bm.preamend
  rebasing 2:2d6884e15790 "b"
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/2d6884e15790-909076cb-backup.hg (glob)
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/7635008c16e1-65f65ff6-preamend-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  o  6ba7926ba204 b
  |
  @  0889a0030a17 aa bm
  |
  o  cb9a9f314b8b a
  
  $ hg bookmarks
   * bm                        1:0889a0030a17

Test that unbookmarked re-amends work well

  $ hg boo -d bm
  $ echo a >> a
  $ hg amend
  
      +----------------------------------------+
      | Please read the Dex article on stacked |
      | diff workflows to understand how the   |
      | fbamend extension works:               |
      |                                        |
      |      https://fburl.com/hgstacks        |
      +----------------------------------------+
  
  warning: the commit's children were left behind
  (use 'hg amend --fixup' to rebase them)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  94eb429c9465 aa
  |
  | o  6ba7926ba204 b
  | |
  | o  0889a0030a17 aa 94eb429c9465.preamend
  |/
  o  cb9a9f314b8b a
  
  $ echo a >> a
  $ hg amend
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/94eb429c9465-30a7ee2c-amend-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  83455f1f6049 aa
  |
  | o  6ba7926ba204 b
  | |
  | o  0889a0030a17 aa 83455f1f6049.preamend
  |/
  o  cb9a9f314b8b a
  
  $ hg amend --fixup
  rebasing the children of 83455f1f6049.preamend
  rebasing 2:6ba7926ba204 "b"
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/6ba7926ba204-9ac223ef-backup.hg (glob)
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/0889a0030a17-6bebea0c-preamend-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  o  455e4104f605 b
  |
  @  83455f1f6049 aa
  |
  o  cb9a9f314b8b a
  

Test interaction with histedit

  $ echo '[extensions]' >> $HGRCPATH
  $ echo "histedit=" >> $HGRCPATH
  $ echo "fbhistedit=" >> $HGRCPATH
  $ hg up tip
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ echo c >> c
  $ hg add c
  $ hg commit -m c
  $ hg log -T '{node|short} {desc}\n'
  765b28efbe8b c
  455e4104f605 b
  83455f1f6049 aa
  cb9a9f314b8b a
  $ hg histedit .^^ --commands - <<EOF
  > pick 83455f1f6049
  > x echo amending from exec
  > x hg commit --amend -m 'message from exec'
  > stop 455e4104f605
  > pick 765b28efbe8b
  > EOF
  0 files updated, 0 files merged, 2 files removed, 0 files unresolved
  amending from exec
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  warning: the commit's children were left behind
  (this is okay since a histedit is in progress)
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  Changes commited as a2329fab3fab. You may amend the commit now.
  When you are finished, run hg histedit --continue to resume
  [1]
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  a2329fab3fab b
  |
  o  048e86baa19d message from exec
  |
  | o  765b28efbe8b c
  | |
  | o  455e4104f605 b
  | |
  | o  83455f1f6049 aa
  |/
  o  cb9a9f314b8b a
  
  $ hg amend --rebase
  abort: histedit in progress
  (during histedit, use amend without --rebase)
  [255]
  $ hg commit --amend -m 'commit --amend message'
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/a2329fab3fab-e6fb940f-amend-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  3166f3b5587d commit --amend message
  |
  o  048e86baa19d message from exec
  |
  | o  765b28efbe8b c
  | |
  | o  455e4104f605 b
  | |
  | o  83455f1f6049 aa
  |/
  o  cb9a9f314b8b a
  
  $ hg histedit --continue
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/83455f1f6049-922a304e-backup.hg (glob)
  $ hg log -G -T '{node|short} {desc} {bookmarks}\n'
  @  0f83a9508203 c
  |
  o  3166f3b5587d commit --amend message
  |
  o  048e86baa19d message from exec
  |
  o  cb9a9f314b8b a
  
Test that --message is respected

  $ hg amend
  nothing changed
  [1]
  $ hg amend --message foo
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/0f83a9508203-7d2a99ee-amend-backup.hg (glob)
  $ hg amend -m bar
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/29272a1da891-35a82ce4-amend-backup.hg (glob)
  $ hg amend
  nothing changed
  [1]

Test that --addremove/-A works

  $ echo new > new
  $ hg amend -A
  adding new
  saved backup bundle to $TESTTMP/repo/.hg/strip-backup/772f45f5a69d-90a7bd63-amend-backup.hg (glob)

Test that the extension disables itself when evolution is enabled

  $ cat > ${TESTTMP}/obs.py << EOF
  > import mercurial.obsolete
  > mercurial.obsolete._enabled = True
  > EOF
  $ echo '[extensions]' >> $HGRCPATH
  $ echo "obs=${TESTTMP}/obs.py" >> $HGRCPATH

noisy warning

  $ hg version 2>&1
  fbamend and evolve extension are imcompatible, fbamend deactivated.
  You can either disable it globally:
  - type `hg config --edit`
  - drop the `fbamend=` line from the `[extensions]` section
  or disable it for a specific repo:
  - type `hg config --local --edit`
  - add a `fbamend=!$TESTTMP/fbamend.py` line in the `[extensions]` section
  Mercurial Distributed SCM (version *) (glob)
  (see http://mercurial.selenic.com for more information)
  
  Copyright (C) 2005-2015 Matt Mackall and others
  This is free software; see the source for copying conditions. There is NO
  warranty; not even for MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

commit has no new flags

  $ hg help commit 2> /dev/null | grep -- --fixup
  [1]
  $ hg help commit 2> /dev/null | grep -- --rebase
  [1]

The amend command is missing

  $ hg help amend
  fbamend and evolve extension are imcompatible, fbamend deactivated.
  You can either disable it globally:
  - type `hg config --edit`
  - drop the `fbamend=` line from the `[extensions]` section
  or disable it for a specific repo:
  - type `hg config --local --edit`
  - add a `fbamend=!$TESTTMP/fbamend.py` line in the `[extensions]` section
  abort: no such help topic: amend
  (try "hg help --keyword amend")
  [255]
