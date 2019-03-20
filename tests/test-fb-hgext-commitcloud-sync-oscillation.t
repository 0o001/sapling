  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > amend =
  > commitcloud =
  > infinitepush =
  > infinitepushbackup =
  > rebase =
  > remotenames =
  > share =
  > [ui]
  > ssh = python "$TESTDIR/dummyssh"
  > [infinitepush]
  > branchpattern = re:scratch/.*
  > [commitcloud]
  > hostname = testhost
  > [experimental]
  > evolution = createmarkers, allowunstable
  > EOF

  $ hg init server
  $ cd server
  $ cat >> .hg/hgrc << EOF
  > [infinitepush]
  > server = yes
  > indextype = disk
  > storetype = disk
  > reponame = testrepo
  > EOF
  $ touch base
  $ hg commit -Aqm base
  $ hg phase -p .
  $ cd ..

Make shared part of config
  $ cat >> shared.rc << EOF
  > [commitcloud]
  > servicetype = local
  > servicelocation = $TESTTMP
  > user_token_path = $TESTTMP
  > auth_help = visit https://localhost/oauth to generate a registration token
  > education_page = https://someurl.com/wiki/CommitCloud
  > owner_team = The Test Team @ FB
  > EOF

Utility script to dump commit cloud metadata
  $ cat > dumpcommitcloudmetadata.py <<EOF
  > import json
  > ccmd = json.load(open("$TESTTMP/commitcloudservicedb"))
  > print("version: %s" % ccmd["version"])
  > print("bookmarks:")
  > for bookmark, node in sorted(ccmd["bookmarks"].items()):
  >    print("    %s => %s" % (bookmark, node))
  > print("heads:")
  > for head in ccmd["heads"]:
  >    print("    %s" % head)
  > EOF

Make a clone of the server
  $ hg clone ssh://user@dummy/server client1 -q
  $ cd client1
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud auth -t xxxxxx
  setting authentication token
  authentication successful

Connect the first client
  $ hg cloud join
  #commitcloud this repository is now connected to the 'user/test/default' workspace for the 'server' repo
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)

Make some commits
  $ hg debugdrawdag <<EOS
  > C E G
  > | | |
  > B D F
  >  \|/
  >   A
  >   |
  >   0
  > EOS
  $ hg cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  backing up stack rooted at 04b96a2be922
  remote: pushing 7 commits:
  remote:     04b96a2be922  A
  remote:     14bec91a4bc5  B
  remote:     449486ddff7a  D
  remote:     64b4d9634423  F
  remote:     65299708466c  C
  remote:     27ad02806080  E
  remote:     878302dcadc7  G
  #commitcloud commits synchronized
  finished in * (glob)
  $ tglogp
  o  7: 878302dcadc7 draft 'G'
  |
  | o  6: 27ad02806080 draft 'E'
  | |
  | | o  5: 65299708466c draft 'C'
  | | |
  o | |  4: 64b4d9634423 draft 'F'
  | | |
  | o |  3: 449486ddff7a draft 'D'
  |/ /
  | o  2: 14bec91a4bc5 draft 'B'
  |/
  o  1: 04b96a2be922 draft 'A'
  |
  @  0: df4f53cec30a public 'base'
  

Create a new client that isn't connected yet
  $ cd ..
  $ hg clone ssh://user@dummy/server client2 -q
  $ cat shared.rc >> client2/.hg/hgrc

Share commits A B C D and E into the repo manually with a bundle
  $ hg bundle -q -R client1 --base 0 -r 'A+B+C+D+E' ABCDE.hg
  $ hg unbundle -R client2 ABCDE.hg
  adding changesets
  adding manifests
  adding file changes
  added 5 changesets with 5 changes to 5 files (+1 heads)
  new changesets 04b96a2be922:27ad02806080
  (run 'hg heads' to see heads, 'hg merge' to merge)
  $ cd client2
  $ tglogp
  o  5: 27ad02806080 draft 'E'
  |
  | o  4: 65299708466c draft 'C'
  | |
  o |  3: 449486ddff7a draft 'D'
  | |
  | o  2: 14bec91a4bc5 draft 'B'
  |/
  o  1: 04b96a2be922 draft 'A'
  |
  @  0: df4f53cec30a public 'base'
  

Hide commits C D and E without the commitcloud extension enabled
  $ hg hide 3 4 5 --config extensions.commitcloud=!
  hiding commit 449486ddff7a "D"
  hiding commit 65299708466c "C"
  hiding commit 27ad02806080 "E"
  3 changesets hidden

Connect to commit cloud
  $ hg cloud join
  #commitcloud this repository is now connected to the 'user/test/default' workspace for the 'server' repo
  #commitcloud synchronizing 'server' with 'user/test/default'
  pulling from ssh://user@dummy/server
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 2 changes to 3 files (+1 heads)
  new changesets 64b4d9634423:878302dcadc7
  (run 'hg heads' to see heads, 'hg merge' to merge)
  #commitcloud commits synchronized
  finished in * (glob)

Syncing in the two repos causes the commits to be revived, and the cloud
workspace does not oscillate between the two views.

  $ cd ..
  $ hg -R client1 cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ python dumpcommitcloudmetadata.py
  version: 2
  bookmarks:
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c
  $ hg -R client2 cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ python dumpcommitcloudmetadata.py
  version: 2
  bookmarks:
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c
  $ hg -R client1 cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ python dumpcommitcloudmetadata.py
  version: 2
  bookmarks:
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c
  $ hg -R client2 cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ python dumpcommitcloudmetadata.py
  version: 2
  bookmarks:
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

Smartlogs match

  $ cd client1
  $ tglogp
  o  7: 878302dcadc7 draft 'G'
  |
  | o  6: 27ad02806080 draft 'E'
  | |
  | | o  5: 65299708466c draft 'C'
  | | |
  o | |  4: 64b4d9634423 draft 'F'
  | | |
  | o |  3: 449486ddff7a draft 'D'
  |/ /
  | o  2: 14bec91a4bc5 draft 'B'
  |/
  o  1: 04b96a2be922 draft 'A'
  |
  @  0: df4f53cec30a public 'base'
  
  $ cd ../client2
  $ tglogp
  o  7: 878302dcadc7 draft 'G'
  |
  o  6: 64b4d9634423 draft 'F'
  |
  | o  5: 27ad02806080 draft 'E'
  | |
  | | o  4: 65299708466c draft 'C'
  | | |
  | o |  3: 449486ddff7a draft 'D'
  |/ /
  | o  2: 14bec91a4bc5 draft 'B'
  |/
  o  1: 04b96a2be922 draft 'A'
  |
  @  0: df4f53cec30a public 'base'
  

Make a new public commit
  $ cd ../server
  $ echo data >> base
  $ hg commit -m 'next'
  $ hg phase -p .

Pull it into one client
  $ cd ../client1
  $ hg pull
  pulling from ssh://user@dummy/server
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files (+1 heads)
  new changesets 5817a557f93f
  (run 'hg heads .' to see heads, 'hg merge' to merge)
  $ tglogp
  o  8: 5817a557f93f public 'next'
  |
  | o  7: 878302dcadc7 draft 'G'
  | |
  | | o  6: 27ad02806080 draft 'E'
  | | |
  | | | o  5: 65299708466c draft 'C'
  | | | |
  | o | |  4: 64b4d9634423 draft 'F'
  | | | |
  | | o |  3: 449486ddff7a draft 'D'
  | |/ /
  | | o  2: 14bec91a4bc5 draft 'B'
  | |/
  | o  1: 04b96a2be922 draft 'A'
  |/
  @  0: df4f53cec30a public 'base'
  

Put a bookmark on the new public commit
  $ hg book foo -r tip
  $ tglogp -r tip
  o  8: 5817a557f93f public 'next' foo
  |
  ~
  $ hg cloud sync -q
  $ python $TESTTMP/dumpcommitcloudmetadata.py
  version: 3
  bookmarks:
      foo => 5817a557f93f46ab290e8571c89624ff856130c0
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

  $ cd ../client2
  $ hg cloud sync -q
  5817a557f93f46ab290e8571c89624ff856130c0 not found, omitting foo bookmark
  $ tglogp -r tip
  o  7: 878302dcadc7 draft 'G'
  |
  ~

  $ python $TESTTMP/dumpcommitcloudmetadata.py
  version: 3
  bookmarks:
      foo => 5817a557f93f46ab290e8571c89624ff856130c0
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

  $ cd ../client1
  $ hg cloud sync -q
  $ tglogp -r tip
  o  8: 5817a557f93f public 'next' foo
  |
  ~

  $ python $TESTTMP/dumpcommitcloudmetadata.py
  version: 3
  bookmarks:
      foo => 5817a557f93f46ab290e8571c89624ff856130c0
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

  $ cd ../client2
  $ hg pull -q
  $ hg cloud sync -q
  $ tglogp -r tip
  o  8: 5817a557f93f public 'next' foo
  |
  ~

  $ python $TESTTMP/dumpcommitcloudmetadata.py
  version: 3
  bookmarks:
      foo => 5817a557f93f46ab290e8571c89624ff856130c0
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      27ad028060800678c2de95fea2e826bbd4bf2c21
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

Ensure everything is synced

  $ cd ../client1
  $ hg pull -q
  $ hg cloud sync -q
  $ cd ../client2
  $ hg pull -q
  $ hg cloud sync -q

Create a commit that was obsoleted without the commitcloud extension loaded, but is bookmarked.

  $ hg hide 5 --config extensions.commitcloud=!
  hiding commit 27ad02806080 "E"
  1 changesets hidden
  $ hg book --hidden -r 5 hiddenbook
  $ tglogp -r 3::
  x  5: 27ad02806080 draft 'E' hiddenbook
  |
  o  3: 449486ddff7a draft 'D'
  |
  ~
  $ hg cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ tglogp -r 3::
  x  5: 27ad02806080 draft 'E' hiddenbook
  |
  o  3: 449486ddff7a draft 'D'
  |
  ~
  $ python $TESTTMP/dumpcommitcloudmetadata.py
  version: 4
  bookmarks:
      foo => 5817a557f93f46ab290e8571c89624ff856130c0
      hiddenbook => 27ad028060800678c2de95fea2e826bbd4bf2c21
  heads:
      65299708466caa8f13c05d82e76d611c183defee
      878302dcadc7a800f326d8e06a5e9beec77e5a1c

Client 1 keeps trying to upload it.

  $ cd ../client1
  $ hg cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  backing up stack rooted at 04b96a2be922
  remote: pushing 3 commits:
  remote:     04b96a2be922  A
  remote:     449486ddff7a  D
  remote:     27ad02806080  E
  #commitcloud commits synchronized
  finished in * (glob)
  $ tglogp -r 3::
  o  6: 27ad02806080 draft 'E' hiddenbook
  |
  o  3: 449486ddff7a draft 'D'
  |
  ~

  $ cd ../client2
  $ hg cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  #commitcloud commits synchronized
  finished in * (glob)
  $ tglogp -r 3::
  x  5: 27ad02806080 draft 'E' hiddenbook
  |
  o  3: 449486ddff7a draft 'D'
  |
  ~

  $ cd ../client1
  $ hg cloud sync
  #commitcloud synchronizing 'server' with 'user/test/default'
  backing up stack rooted at 04b96a2be922
  remote: pushing 3 commits:
  remote:     04b96a2be922  A
  remote:     449486ddff7a  D
  remote:     27ad02806080  E
  #commitcloud commits synchronized
  finished in * (glob)
  $ tglogp -r 3::
  o  6: 27ad02806080 draft 'E' hiddenbook
  |
  o  3: 449486ddff7a draft 'D'
  |
  ~

