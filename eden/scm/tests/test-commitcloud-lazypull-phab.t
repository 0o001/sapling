#chg-compatible

  $ configure dummyssh mutation-norecord
  $ disable treemanifest
  $ enable amend arcdiff commitcloud infinitepush rebase remotenames share
  $ setconfig extensions.arcconfig="$TESTDIR/../edenscm/hgext/extlib/phabricator/arcconfig.py"
  $ setconfig infinitepush.branchpattern="re:scratch/.*" commitcloud.hostname=testhost
  $ readconfig <<EOF
  > [alias]
  > trglog = log -G --template "{node|short} '{desc}' {bookmarks} {remotenames}\n"
  > descr = log -r '.' --template "{desc}"
  > EOF

  $ setconfig remotefilelog.reponame=server

  $ mkcommit() {
  >   echo "$1" > "$1"
  >   hg commit -Aqm "$1"
  > }

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

Make shared part of config
  $ cat >> shared.rc << EOF
  > [commitcloud]
  > servicetype = local
  > servicelocation = $TESTTMP
  > user_token_path = $TESTTMP
  > owner_team = The Test Team @ FB
  > EOF

Make the first clone of the server
  $ hg clone ssh://user@dummy/server client1 -q
  $ cd client1
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud auth -t xxxxxx -q
  $ hg cloud join -q

  $ cd ..

Make the second clone of the server
  $ hg clone ssh://user@dummy/server client2 -q
  $ cd client2
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud auth -q
  $ hg cloud join -q

  $ cd ..

Make the third clone of the server
  $ hg clone ssh://user@dummy/server client3 -q
  $ cd client3
  $ cat ../shared.rc >> .hg/hgrc
  $ hg cloud auth -q
  $ hg cloud join -q

  $ cd ..

Test for `hg diff --since-last-submit`

  $ cd client1
  $ echo '{}' > .arcrc
  $ echo '{"config" : {"default" : "https://a.com/api"}, "hosts" : {"https://a.com/api/" : { "user" : "testuser", "cert" : "garbage_cert"}}}' > .arcconfig

  $ cd ..

  $ cd client2
  $ echo '{}' > .arcrc
  $ echo '{"config" : {"default" : "https://a.com/api"}, "hosts" : {"https://a.com/api/" : { "user" : "testuser", "cert" : "garbage_cert"}}}' > .arcconfig

  $ cd ..

  $ cd client3
  $ echo '{}' > .arcrc
  $ echo '{"config" : {"default" : "https://a.com/api"}, "hosts" : {"https://a.com/api/" : { "user" : "testuser", "cert" : "garbage_cert"}}}' > .arcconfig

  $ cd ..

  $ cd client1

  $ echo "Hello feature2" > feature2.body.txt
  $ hg add feature2.body.txt

  $ hg ci -Aqm 'Differential Revision: https://phabricator.fb.com/D1'
  $ hg log -r '.' -T '{node}'
  a8080066a666ffa51c0a171e87d5a0396ecb559a (no-eol)
  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at a8080066a666
  commitcloud: commits synchronized
  finished in * (glob)
  remote: pushing 1 commit:
  remote:     a8080066a666  Differential Revision: https://phabricator.fb.com/

  $ cat > $TESTTMP/mockduit << EOF
  > [{"data": {"query": [{"results": {"nodes": [{
  >   "number": 1,
  >   "diff_status_name": "Needs Review",
  >   "latest_active_diff": {
  >     "local_commit_info": {
  >       "nodes": [
  >         {"property_value": "{\"lolwut\": {\"time\": 0, \"commit\": \"a8080066a666ffa51c0a171e87d5a0396ecb559a\"}}"}
  >       ]
  >     }
  >   },
  >   "differential_diffs": {"count": 1},
  >   "is_landing": false,
  >   "created_time": 123,
  >   "updated_time": 222
  > }]}}]}}]
  > EOF

  $ echo "Hello feature2 update" > feature2.body.txt
  $ hg amend

  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  backing up stack rooted at 95847be64d6a
  commitcloud: commits synchronized
  finished in * (glob)
  remote: pushing 1 commit:
  remote:     95847be64d6a  Differential Revision: https://phabricator.fb.com/

  $ cd ..

  $ cd client2

  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  pulling 95847be64d6a from ssh://user@dummy/server
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 3 changes to 3 files
  commitcloud: commits synchronized
  finished in * (glob)

  $ hg up 95847be64d6a
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved

  $ HG_ARC_CONDUIT_MOCK=$TESTTMP/mockduit hg diff --since-last-submit --config extensions.commitcloud=!
  pulling 'a8080066a666ffa51c0a171e87d5a0396ecb559a' from 'ssh://user@dummy/server'
  diff -r a8080066a666 -r 95847be64d6a feature2.body.txt
  --- a/feature2.body.txt	Thu Jan 01 00:00:00 1970 +0000
  +++ b/feature2.body.txt	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +1,1 @@
  -Hello feature2
  +Hello feature2 update

  $ HG_ARC_CONDUIT_MOCK=$TESTTMP/mockduit hg diff --since-last-submit
  pulling 'a8080066a666ffa51c0a171e87d5a0396ecb559a' from 'ssh://user@dummy/server'
  diff -r a8080066a666 -r 95847be64d6a feature2.body.txt
  --- a/feature2.body.txt	Thu Jan 01 00:00:00 1970 +0000
  +++ b/feature2.body.txt	Thu Jan 01 00:00:00 1970 +0000
  @@ -1,1 +1,1 @@
  -Hello feature2
  +Hello feature2 update

  $ cd ..

  $ cd client3

  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  pulling 95847be64d6a from ssh://user@dummy/server
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 3 changes to 3 files
  commitcloud: commits synchronized
  finished in * (glob)

  $ hg up 95847be64d6a
  3 files updated, 0 files merged, 0 files removed, 0 files unresolved

  $ HG_ARC_CONDUIT_MOCK=$TESTTMP/mockduit hg log -r 'lastsubmitted(.)' -T '{node} {desc}'  --config extensions.commitcloud=!
  pulling 'a8080066a666ffa51c0a171e87d5a0396ecb559a' from 'ssh://user@dummy/server'
  a8080066a666ffa51c0a171e87d5a0396ecb559a Differential Revision: https://phabricator.fb.com/D1 (no-eol)

  $ HG_ARC_CONDUIT_MOCK=$TESTTMP/mockduit hg log --hidden -r 'lastsubmitted(.)' -T '{node} {desc}'
  pulling 'a8080066a666ffa51c0a171e87d5a0396ecb559a' from 'ssh://user@dummy/server'
  a8080066a666ffa51c0a171e87d5a0396ecb559a Differential Revision: https://phabricator.fb.com/D1 (no-eol)
