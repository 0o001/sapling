#chg-compatible

  $ configure modern
  $ setconfig infinitepush.branchpattern=re:scratch/.+

  $ showgraph() {
  >    hg log -G -T "{desc}: {phase} {bookmarks} {remotenames}" -r "all()"
  > }

  $ newserver server
  $ cd $TESTTMP/server
  $ echo base > base
  $ hg commit -Aqm base
  $ echo 1 > public1
  $ hg commit -Aqm public1
  $ hg bookmark master
  $ hg prev -q
  [d20a80] base
  $ echo 2 > public2
  $ hg commit -Aqm public2
  $ hg bookmark other

  $ cd $TESTTMP
  $ clone server client1
  $ cd client1
  $ hg up -q remote/master
  $ hg cloud sync -q
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public
  

  $ cd $TESTTMP
  $ clone server client2
  $ cd client2
  $ hg up -q remote/master
  $ hg cloud sync -q
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public
  

  $ cd $TESTTMP
  $ clone server client3
  $ cd client3
  $ hg up -q remote/master
  $ hg cloud sync -q
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public
  

  $ cd $TESTTMP
  $ clone server client4
  $ cd client4
  $ hg up -q remote/master
  $ hg cloud sync -q
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public
  

Pull the other bookmark so we have a subscription.
  $ cd $TESTTMP/client1
  $ hg pull -B other
  pulling from ssh://user@dummy/server
  no changes found
  $ hg book --list-subs
     remote/master             1:9da34b1aa207
     remote/other              2:4c8ee072cf16
  $ hg up -q 0

Push to a new public branch
  $ echo 3 > public3
  $ hg commit -Aqm public3
  $ hg push --to created --create
  pushing rev ec1dff19c429 to destination ssh://user@dummy/server bookmark created
  searching for changes
  exporting bookmark created
  remote: adding changesets
  remote: adding manifests
  remote: adding file changes
  remote: added 1 changesets with 1 changes to 1 files
  $ hg book --list-subs
     remote/master             1:9da34b1aa207
     remote/other              2:4c8ee072cf16
  $ showgraph
  @  public3: draft
  |
  | o  public2: public  remote/other
  |/
  | o  public1: public  remote/master
  |/
  o  base: public
  

BUG! public3 is draft and 'created' is not subscribed to

Workaround this bug by pulling created
  $ hg pull -B created
  pulling from ssh://user@dummy/server
  no changes found
  $ showgraph
  @  public3: public  remote/created
  |
  | o  public2: public  remote/other
  |/
  | o  public1: public  remote/master
  |/
  o  base: public
  

Create a draft commit and push it to a scratch branch
  $ echo 1 > draft1
  $ hg commit -Aqm draft1
  $ hg push --to scratch/draft1 --create
  pushing to ssh://user@dummy/server
  searching for changes
  remote: pushing 1 commit:
  remote:     d860d2fc26c5  draft1
  $ hg cloud sync -q
  $ hg up 0
  0 files updated, 0 files merged, 2 files removed, 0 files unresolved
  $ showgraph
  o  draft1: draft  remote/scratch/draft1
  |
  o  public3: public  remote/created
  |
  | o  public2: public  remote/other
  |/
  | o  public1: public  remote/master
  |/
  @  base: public
  
  $ hg cloud sync -q

  $ cd $TESTTMP/client2
  $ hg cloud sync -q
  $ hg book --list-subs
     remote/created            3:ec1dff19c429
     remote/master             1:9da34b1aa207
     remote/other              2:4c8ee072cf16
     remote/scratch/draft1     4:d860d2fc26c5
  $ showgraph
  o  draft1: draft  remote/scratch/draft1
  |
  o  public3: public  remote/created
  |
  | o  public2: public  remote/other
  |/
  | @  public1: public  remote/master
  |/
  o  base: public
  

Pull in this repo
  $ hg pull
  pulling from ssh://user@dummy/server
  no changes found
  $ showgraph
  o  draft1: draft  remote/scratch/draft1
  |
  o  public3: draft
  |
  | @  public1: public  remote/master
  |/
  o  base: public
  
BUG! our subscriptions have been lost

Work around this by pulling them by name
  $ hg pull -B created -B other
  pulling from ssh://user@dummy/server
  no changes found
  $ showgraph
  o  draft1: draft  remote/scratch/draft1
  |
  o  public3: public  remote/created
  |
  | o  public2: public  remote/other
  |/
  | @  public1: public  remote/master
  |/
  o  base: public
  

Sync in the third repo
  $ cd $TESTTMP/client3
  $ hg cloud sync -q
  $ hg book --list-subs
     remote/created            3:ec1dff19c429
     remote/master             1:9da34b1aa207
     remote/other              2:4c8ee072cf16
     remote/scratch/draft1     4:d860d2fc26c5
  $ showgraph
  o  draft1: draft  remote/scratch/draft1
  |
  o  public3: public  remote/created
  |
  | o  public2: public  remote/other
  |/
  | @  public1: public  remote/master
  |/
  o  base: public
  

Remove these remote bookmarks

  $ hg hide remote/scratch/draft1
  hiding commit d860d2fc26c5 "draft1"
  1 changeset hidden
TODO: make this a command
  $ hg debugshell -c "with repo.wlock(), repo.lock(), repo.transaction(\"deleteremotebookmarks\"): repo._remotenames.applychanges({\"bookmarks\": {key: '0'*40 if key in {'remote/other', 'remote/created'} else edenscm.mercurial.node.hex(value[0]) for key, value in repo._remotenames[\"bookmarks\"].items() }})"
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public
  
  $ hg cloud sync -q

Sync in the first repo
  $ cd $TESTTMP/client1
  $ hg cloud sync -q
  $ hg book --list-subs
     remote/master             1:9da34b1aa207
     remote/scratch/draft1     4:d860d2fc26c5
  $ showgraph
  o  public1: public  remote/master
  |
  @  base: public
  

Make an unrelated change to the cloud workspace and sync again
  $ hg book local
  $ hg cloud sync -q

Sync in the third repo again
  $ cd $TESTTMP/client3
  $ hg cloud sync -q
  $ hg book --list-subs
     remote/master             1:9da34b1aa207
     remote/scratch/draft1     4:d860d2fc26c5
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public local
  
  $ hg pull
  pulling from ssh://user@dummy/server
  no changes found
  $ hg book --list-subs
     remote/master             1:9da34b1aa207
     remote/scratch/draft1     4:d860d2fc26c5
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public local
  

Sync in the fourth repo
  $ cd $TESTTMP/client4
  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  commitcloud: commits synchronized
  finished in * sec (glob)

  $ hg book --list-subs
     remote/master             1:9da34b1aa207
  $ showgraph
  @  public1: public  remote/master
  |
  o  base: public local
  

Sync in the second repo with one of the deleted bookmarks protected
  $ cd $TESTTMP/client2
  $ setconfig remotenames.selectivepulldefault="master, other"
  $ hg cloud sync
  commitcloud: synchronizing 'server' with 'user/test/default'
  commitcloud: commits synchronized
  finished in 0.00 sec
  $ showgraph
  o  public2: public  remote/other
  |
  | @  public1: public  remote/master
  |/
  o  base: public local
  

The other bookmark is now revived in the other repos
  $ cd $TESTTMP/client4
  $ hg cloud sync -q
  $ showgraph
  o  public2: public  remote/other
  |
  | @  public1: public  remote/master
  |/
  o  base: public local
  
