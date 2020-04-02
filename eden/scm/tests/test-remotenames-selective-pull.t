#chg-compatible

  $ configure dummyssh
  $ disable treemanifest
  $ enable remotenames

Set up repos
  $ hg init remoterepo
  $ hg clone -q ssh://user@dummy/remoterepo localrepo

Pull master bookmark

  $ cd remoterepo
  $ echo a > a
  $ hg add a
  $ hg commit -m 'First'
  $ hg book master
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  requesting all changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  $ hg bookmarks --list-subscriptions
     default/master            0:1449e7934ec1

Set up selective pull
  $ setconfig remotenames.selectivepull=True
  $ setconfig remotenames.selectivepullaccessedbookmarks=True
  $ setconfig remotenames.selectivepulldefault=master

Create another bookmark on the remote repo
  $ cd ../remoterepo
  $ hg up null
  0 files updated, 0 files merged, 1 files removed, 0 files unresolved
  (leaving bookmark master)
  $ hg book secondbook
  $ echo b >> a
  $ hg add a
  $ hg commit -m 'Commit secondbook points to'

Do not pull new boookmark from local repo
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  no changes found
  $ hg bookmarks --list-subscriptions
     default/master            0:1449e7934ec1

Do not pull new bookmark even if it on the same commit as old bookmark
  $ cd ../remoterepo
  $ hg up -q master
  $ hg book thirdbook
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  no changes found
  $ hg bookmarks --list-subscriptions
     default/master            0:1449e7934ec1

Move master bookmark
  $ cd ../remoterepo
  $ hg up -q master
  $ echo a >> a
  $ hg commit -m 'Move master bookmark'
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  $ hg bookmarks --list-subscriptions
     default/master            1:0238718db2b1

Specify bookmark to pull
  $ hg pull -B secondbook
  pulling from ssh://user@dummy/remoterepo
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  $ hg bookmarks --list-subscriptions
     default/master            1:0238718db2b1
     default/secondbook        2:ed7a9fd254d1

Create second remote
  $ cd ..
  $ hg clone -q ssh://user@dummy/remoterepo secondremoterepo
  $ cd secondremoterepo
  $ hg up -q 0238718db2b1
  $ hg book master --force
  $ cd ..

Add second remote repo path in localrepo
  $ cd localrepo
  $ setglobalconfig paths.secondremote="ssh://user@dummy/secondremoterepo"
  $ hg pull secondremote
  pulling from ssh://user@dummy/secondremoterepo
  no changes found
  $ hg book --list-subscriptions
     default/master            1:0238718db2b1
     default/secondbook        2:ed7a9fd254d1
     secondremote/master       1:0238718db2b1

Move bookmark in second remote, pull and make sure it doesn't move in local repo
  $ cd ../secondremoterepo
  $ hg book secondbook --force
  $ echo aaa >> a
  $ hg commit -m 'Move bookmark in second remote'
  $ cd ../localrepo
  $ hg pull secondremote
  pulling from ssh://user@dummy/secondremoterepo
  no changes found

Move bookmark in first remote, pull and make sure it moves in local repo
  $ cd ../remoterepo
  $ hg up -q secondbook
  $ echo bbb > a
  $ hg commit -m 'Moves second bookmark'
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  $ hg bookmarks --list-subscriptions
     default/master            1:0238718db2b1
     default/secondbook        3:c47dca9795c9
     secondremote/master       1:0238718db2b1

Delete bookmark on the server
  $ cd ../remoterepo
  $ hg book -d secondbook
  $ cd ../localrepo
  $ hg pull
  pulling from ssh://user@dummy/remoterepo
  no changes found
  $ hg bookmarks --list-subscriptions
     default/master            1:0238718db2b1
     secondremote/master       1:0238718db2b1

Update to the remote bookmark
  $ hg update thirdbook
  `thirdbook` not found: assuming it is a remote bookmark and trying to pull it
  pulling from ssh://user@dummy/remoterepo
  no changes found
  `thirdbook` found remotely
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg book --verbose
  no bookmarks set
  $ hg book --list-subscriptions
     default/master            1:0238718db2b1
     default/thirdbook         0:1449e7934ec1
     secondremote/master       1:0238718db2b1

Trying to update to unknown bookmark
  $ hg update unknownbook
  `unknownbook` not found: assuming it is a remote bookmark and trying to pull it
  pulling from ssh://user@dummy/remoterepo
  pull failed: remote bookmark unknownbook not found!
  abort: unknown revision 'unknownbook'!
  (if unknownbook is a remote bookmark or commit, try to 'hg pull' it first)
  [255]

Update to the remote bookmark from secondremote
  $ hg update secondremote/secondbook
  `secondremote/secondbook` not found: assuming it is a remote bookmark and trying to pull it
  pulling from ssh://user@dummy/secondremoterepo
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  `secondremote/secondbook` found remotely
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg book --list-subscriptions
     default/master            1:0238718db2b1
     default/thirdbook         0:1449e7934ec1
     secondremote/master       1:0238718db2b1
     secondremote/secondbook   4:0022441e80e5

Update make sure revsets work
  $ hg up '.^'
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved

Make another clone with selectivepull disabled
  $ cd ..
  $ hg clone -q ssh://user@dummy/remoterepo localrepo2
  $ cd localrepo2
  $ hg book --list-subscriptions
     default/master            2:0238718db2b1
     default/thirdbook         0:1449e7934ec1

Enable selectivepull and make a pull. Make sure only master bookmark is left
  $ setconfig remotenames.selectivepull=True
  $ setconfig remotenames.selectivepullaccessedbookmarks=True
  $ setconfig remotenames.selectivepulldefault=master

  $ hg pull -q
  $ hg book --list-subscriptions
     default/master            2:0238718db2b1

Temporarily disable selectivepull, pull, enable it again and pull again.
Make sure only master bookmark is present
  $ hg pull --config remotenames.selectivepull=False -q
  $ hg book --list-subscriptions
     default/master            2:0238718db2b1
     default/thirdbook         0:1449e7934ec1
  $ hg pull -q
  $ hg book --list-subscriptions
     default/master            2:0238718db2b1

Check that log shows the hint about selective pull
  $ hg log -r thirdbook
  abort: unknown revision 'thirdbook'!
  (if thirdbook is a remote bookmark or commit, try to 'hg pull' it first)
  [255]

By using "default/" the commit gets automatically pulled
  $ hg log -r default/thirdbook
  attempt to pull default/thirdbook
  changeset:   0:1449e7934ec1
  bookmark:    default/thirdbook
  hoistedname: thirdbook
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  summary:     First
  

Set two bookmarks in selectivepulldefault, make sure both of them were pulled
  $ setconfig "remotenames.selectivepulldefault=master,thirdbook"

  $ rm .hg/selectivepullenabled
  $ hg pull -q
  $ hg book --list-subscriptions
     default/master            2:0238718db2b1
     default/thirdbook         0:1449e7934ec1

Check that `--remote` shows real remote bookmarks from default remote

  $ hg book --remote
     default/master                    0238718db2b174d2622ae9c4c75d61745eb12b25
     default/thirdbook                 1449e7934ec1c4d0c2eefb1194c1cb70e78ba232

  $ hg book --remote -Tjson
  [
   {
    "node": "0238718db2b174d2622ae9c4c75d61745eb12b25",
    "remotebookmark": "default/master"
   },
   {
    "node": "1449e7934ec1c4d0c2eefb1194c1cb70e78ba232",
    "remotebookmark": "default/thirdbook"
   }
  ]
  $ hg --config extensions.infinitepush= book --remote --remote-path ssh://user@dummy/secondremoterepo
     secondremote/master                    0238718db2b174d2622ae9c4c75d61745eb12b25
     secondremote/secondbook                0022441e80e5ed8a23872349474506906b9507e0

when selectivepull is disabled

  $ hg book --remote --config remotenames.selectivepull=false
     default/master            2:0238718db2b1
     default/thirdbook         0:1449e7934ec1

  $ hg book --remote --config remotenames.selectivepull=false -Tjson
  [
   {
    "node": "0238718db2b174d2622ae9c4c75d61745eb12b25",
    "remotebookmark": "default/master",
    "rev": 2
   },
   {
    "node": "1449e7934ec1c4d0c2eefb1194c1cb70e78ba232",
    "remotebookmark": "default/thirdbook",
    "rev": 0
   }
  ]

Clone remote repo with the selectivepull enabled
  $ cd ..

  $ hg clone --config remotenames.selectivepull=True --config remotenames.selectivepulldefault=master ssh://user@dummy/remoterepo new_localrepo
  requesting all changes
  adding changesets
  adding manifests
  adding file changes
  added 4 changesets with 4 changes to 1 files
  updating to branch default
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd new_localrepo

  $ setconfig remotenames.selectivepull=True
  $ setconfig remotenames.selectivepullaccessedbookmarks=True
  $ setconfig remotenames.selectivepulldefault=master

  $ hg book --list-subscriptions
     default/master            2:0238718db2b1

Check remote bookmarks after push
  $ hg up master -q
  $ echo "new commit to push" >> pushsh
  $ hg commit -qAm "push commit"
  $ hg push -r . --to master -q
  $ hg book --list-subscriptions
     default/master            4:a81520e7283a

Check the repo.pull API

  $ newrepo
  $ setconfig paths.default=ssh://user@dummy/remoterepo

- Pull nothing != Pull everything
  $ hg debugpull
  $ hg log -Gr 'all()' -T '{node} {desc} {remotenames}'

- Pull a name, and an unknown name. The unknown name is not an error (it will delete the name).
  $ hg debugpull -B thirdbook -B not-found
  $ hg log -Gr 'all()' -T '{node} {desc} {remotenames}'
  o  1449e7934ec1c4d0c2eefb1194c1cb70e78ba232 First default/thirdbook
  
- Only pull 'master' without 'thirdbook'. This will not work if remotenames expull is not bypassed.
  $ newrepo
  $ setconfig paths.default=ssh://user@dummy/remoterepo
  $ hg debugpull -B master
  $ hg log -Gr 'all()' -T '{node} {desc} {remotenames}'
  o  a81520e7283a6967ec1d82620b75ab92f5478638 push commit default/master
  |
  o  0238718db2b174d2622ae9c4c75d61745eb12b25 Move master bookmark
  |
  o  1449e7934ec1c4d0c2eefb1194c1cb70e78ba232 First
  
- Pull by hash + name + prefix
  $ newrepo
  $ setconfig paths.default=ssh://user@dummy/remoterepo
  $ hg debugpull -B thirdbook -r 0238718db2b174d2622ae9c4c75d61745eb12b25 -r 1449e7934ec1c
  $ hg log -Gr 'all()' -T '{node} {desc} {remotenames}'
  o  0238718db2b174d2622ae9c4c75d61745eb12b25 Move master bookmark
  |
  o  1449e7934ec1c4d0c2eefb1194c1cb70e78ba232 First default/thirdbook
  
- Auto pull in revset resolution

  $ newrepo
  $ setconfig paths.default=ssh://user@dummy/remoterepo
  $ hg log -r default/thirdbook::default/master -T '{node} {desc} {remotenames}\n'
  attempt to pull default/thirdbook
  attempt to pull default/master
  1449e7934ec1c4d0c2eefb1194c1cb70e78ba232 First default/thirdbook
  0238718db2b174d2622ae9c4c75d61745eb12b25 Move master bookmark 
  a81520e7283a6967ec1d82620b75ab92f5478638 push commit default/master

