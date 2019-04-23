#require fsmonitor

  $ newrepo
  $ enable fsmonitor
  $ touch a b c d e f
  $ hg ci -m init -A a b c d e f

The fsmonitor extension should bump clock even if there are nothing changed for
treestate, but too many results returned by watchman.

  $ hg status

 (Run status again after 1 second to make sure mtime < fsnow)
  $ sleep 1
  $ hg status

In this case, treestate has 0 files marked NEED_CHECK, but fsmonitor returns
many files:

  $ touch a b c d e f

  $ hg debugstatus
  len(dirstate) = 6
  len(nonnormal) = 0
  len(filtered nonnormal) = 0
  clock = * (glob)

  $ enable blackbox
  $ setconfig blackbox.track=fsmonitor_status,fsmonitor_details,fsmonitor
  $ hg status

  $ hg blackbox | sed "s/^[^>]*> //;s/c:[0-9][0-9:]*/c:x/"
  clock='c:x' len(nonnormal)=0
  setlastclock 'c:x'
  setlastisfresh False
  watchman returned ['a', 'b', 'c', 'd', 'e', 'f']
  getlastclock 'c:x'
  set clock='c:x' notefiles=[]
  status exited 0 after 0.00 seconds

The watchman clock remains unchanged. Watchman still returns 4 files, which
means the "status" command could still be slow.

  $ rm .hg/blackbox*
  $ hg status
  $ hg blackbox | grep watchman | sed "s/^[^>]*> //;s/c:[0-9][0-9:]*/c:x/"
  watchman returned ['a', 'b', 'c', 'd', 'e', 'f']

With watchman-changed-file-threshold set, clock is bumped and watchman can
return an empty list:

  $ hg status
  $ setconfig fsmonitor.watchman-changed-file-threshold=5

  $ rm .hg/blackbox*
  $ hg status
  $ hg blackbox | grep watchman | sed "s/^[^>]*> //;s/c:[0-9][0-9:]*/c:x/"
  watchman returned ['a', 'b', 'c', 'd', 'e', 'f']

  $ sleep 1

  $ rm .hg/blackbox*
  $ hg status
  $ hg blackbox | grep watchman | sed "s/^[^>]*> //;s/c:[0-9][0-9:]*/c:x/"
  watchman returned []
