#chg-compatible

  $ configure mutation-norecord dummyssh
  $ disable treemanifest
  $ enable amend directaccess commitcloud infinitepush rebase remotenames
  $ setconfig remotefilelog.reponame=server

  $ mkcommit() {
  >   echo "$1" > "$1"
  >   hg commit -Aqm "$1"
  > }

Make a server
  $ hg init server
  $ cd server
  $ cat >> .hg/hgrc << EOF
  > [infinitepush]
  > server = yes
  > indextype = disk
  > storetype = disk
  > reponame = testrepo
  > EOF

  $ mkcommit "base"
  $ hg bookmark master

  $ cd ..

Make a secondary server
  $ hg clone ssh://user@dummy/server server1 -q
  $ cd server1
  $ cat >> .hg/hgrc << EOF
  > [infinitepush]
  > server = yes
  > indextype = disk
  > storetype = disk
  > reponame = testrepo
  > EOF

  $ cd ..

Make shared part of client config
  $ cat >> shared.rc << EOF
  > [commitcloud]
  > hostname = testhost
  > servicetype = local
  > servicelocation = $TESTTMP
  > token_enforced=False
  > EOF

Make the first clone of the server
  $ hg clone ssh://user@dummy/server client1 -q
  $ cd client1
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud join -q

  $ cd ..

Make the second clone of the server
  $ hg clone ssh://user@dummy/server client2 -q
  $ cd client2
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud join -q

  $ cd ..

Make a commit in the first client, and sync it
  $ cd client1
  $ mkcommit "commit1"
  $ hg cloud sync -q

  $ cd ..

Sync from the second client - the commit should appear
  $ cd client2
  $ hg cloud sync -q

  $ hg up -q tip
  $ tglog
  @  fa5d62c46fd7 'commit1'
  |
  o  d20a80d4def3 'base'
  

Make a commit in the second client, and sync it
  $ mkcommit "commit2"
  $ hg cloud sync -q

  $ cd ..

Return to the first client and configure a different paths.infinitepush
It will push its commit to the new server, but will fail to sync
because it can't access the second commit.

  $ cd client1
  $ mkcommit "commit3"

  $ hg cloud sync --config paths.infinitepush=ssh://user@dummy/server1
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at fa5d62c46fd7
  pulling 02f6fc2b7154 from ssh://user@dummy/server1
  searching for changes
  remote: abort: 02f6fc2b715444d7df09bd859e1d4877f9ef9946 head is not known
  remote: pushing 2 commits:
  remote:     fa5d62c46fd7  commit1
  remote:     26d5a99991bd  commit3
  abort: pull failed on remote
  [255]

  $ cd ..

Return to client2.  We can still sync using the old server.

  $ cd client2
  $ mkcommit "commit4"
  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at fa5d62c46fd7
  commitcloud: commits synchronized
  finished in * (glob)
  remote: pushing 3 commits:
  remote:     fa5d62c46fd7  commit1
  remote:     02f6fc2b7154  commit2
  remote:     c701070be855  commit4

Configure the new server on this client.  It will now send all of its commits.
  $ hg cloud sync --config paths.infinitepush=ssh://user@dummy/server1
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at fa5d62c46fd7
  commitcloud: commits synchronized
  finished in * (glob)
  remote: pushing 3 commits:
  remote:     fa5d62c46fd7  commit1
  remote:     02f6fc2b7154  commit2
  remote:     c701070be855  commit4

  $ cd ..

The first client can now successfully sync using the new server.
  $ cd client1
  $ hg cloud sync --config paths.infinitepush=ssh://user@dummy/server1
  commitcloud: synchronizing 'server' with 'user/test/default'
  pulling c701070be855 from ssh://user@dummy/server1
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 2 changes to 3 files
  commitcloud: commits synchronized
  finished in * (glob)

Switching back to the previous server still works, and the missing commits
are backed up there.
  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at fa5d62c46fd7
  commitcloud: commits synchronized
  finished in * (glob)
  remote: pushing 2 commits:
  remote:     fa5d62c46fd7  commit1
  remote:     26d5a99991bd  commit3

