#chg-compatible

Set up extension and repos
  $ enable remotenames
  $ hg init repo1
  $ cd repo1
  $ echo a > a
  $ hg add a
  $ hg commit -qm 'a'
  $ hg boo bm1
  $ hg boo bm2
  $ cd ..
  $ hg clone repo1 repo2
  updating to branch default
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd repo2
  $ setconfig paths.default-push="$TESTTMP/repo1"
  $ hg pull
  pulling from $TESTTMP/repo1 (glob)
  searching for changes
  no changes found
  $ hg log -l 1 -T '{node|short} {remotenames}\n'
  cb9a9f314b8b default/bm1 default/bm2

Test renaming

  $ hg dbsh -c 'with repo.lock(), repo.transaction("tr"): repo.svfs.writeutf8("remotenames","")'
  $ setglobalconfig remotenames.rename.default=remote
  $ hg pull
  pulling from $TESTTMP/repo1 (glob)
  searching for changes
  no changes found
  $ hg log -l 1 -T '{node|short} {remotenames}\n'
  cb9a9f314b8b remote/bm1 remote/bm2

Test hoisting basics
  $ hg book
  no bookmarks set
  $ hg debugnamecomplete
  default
  remote/bm1
  remote/bm2
  $ setglobalconfig remotenames.hoist=remote
  $ hg debugnamecomplete
  bm1
  bm2
  default
  remote/bm1
  remote/bm2

Test hoisting name lookup
  $ hg dbsh -c 'with repo.lock(), repo.transaction("tr"): repo.svfs.writeutf8("remotenames","")'
  $ hg log -r . -T '{hoistedbookmarks}\n'
  
  $ hg pull
  pulling from $TESTTMP/repo1 (glob)
  searching for changes
  no changes found
  $ hg log -r bm1 -T '{node|short} - {bookmarks} - {hoistednames} - {remotebookmarks}\n'
  cb9a9f314b8b -  - bm1 bm2 - remote/bm1 remote/bm2
  $ hg log -r bm2 -T '{node|short} - {bookmarks} - {hoistednames} - {remotebookmarks}\n'
  cb9a9f314b8b -  - bm1 bm2 - remote/bm1 remote/bm2

Test transition bookmark deletion
  $ hg dbsh -c 'with repo.lock(), repo.transaction("tr"): repo.svfs.writeutf8("remotenames","")'
  $ hg book stable -r .
  $ echo b > b
  $ hg add b
  $ hg commit -qm 'b'
  $ hg book notdeleted
  $ hg book master
  $ hg bookmarks
   * master                    1:d2ae7f538514
     notdeleted                1:d2ae7f538514
     stable                    0:cb9a9f314b8b
  $ setglobalconfig remotenames.transitionbookmarks="master, stable, other"
  $ hg pull
  pulling from $TESTTMP/repo1 (glob)
  searching for changes
  no changes found
  $ hg bookmarks
     notdeleted                1:d2ae7f538514

Test message
  $ hg dbsh -c 'with repo.lock(), repo.transaction("tr"): repo.svfs.writeutf8("remotenames","")'
  $ hg book -ir tip master
  $ readglobalconfig <<EOF
  > [remotenames]
  > transitionmessage = Test transition message
  >                     with newline
  > EOF
  $ hg pull -q
  Test transition message
  with newline

Test transition bookmark disallowed
  $ setglobalconfig remotenames.disallowedbookmarks="master, stable, other, notdelete"
  $ hg book master
  abort: bookmark 'master' not allowed by configuration
  [255]
  $ hg book okay stable
  abort: bookmark 'stable' not allowed by configuration
  [255]
  $ hg book other -r ".^"
  abort: bookmark 'other' not allowed by configuration
  [255]
  $ hg book foo
  $ hg book -m foo stable
  abort: bookmark 'stable' not allowed by configuration
  [255]
  $ hg book -d notdeleted

Test push to renamed dest
  $ hg push remote
  pushing to $TESTTMP/repo1 (glob)
  searching for changes
  abort: push would create new anonymous heads (d2ae7f538514)
  (use --allow-anon to override this warning)
  [255]

Test pull from renamed source
  $ hg pull remote
  pulling from $TESTTMP/repo1 (glob)
  searching for changes
  no changes found

