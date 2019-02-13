Test native objects attached to the "repo" object gets properly released at the
end of process.

Attach an object with `__del__` to learn whether repo, ui are dropped on not.

  $ cat > $TESTTMP/printondel.py << EOF
  > class printondel(object):
  >     def __del__(self):
  >         print("__del__ called")
  > def reposetup(ui, repo):
  >     obj = printondel()
  >     repo._deltest = obj
  >     ui._deltest = obj
  > EOF

  $ setconfig extensions.leakdetect=$TESTTMP/printondel.py

No leak without extensions

  $ newrepo
  __del__ called

  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: blackbox

  $ newrepo
  __del__ called
  $ setconfig extensions.blackbox=
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: remotefilelog

  $ newrepo
  __del__ called
  $ echo remotefilelog >> .hg/requires
  $ setconfig extensions.remotefilelog= remotefilelog.cachepath=$TESTTMP/cache
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: treemanifest and fastmanifest

  $ newrepo
  __del__ called
  $ setconfig extensions.treemanifest= extensions.fastmanifest= remotefilelog.reponame=x
  $ hg log -r . -T '{node}\n'
  0000000000000000000000000000000000000000
  __del__ called
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: treemanifest only

  $ newrepo
  __del__ called
  $ setconfig extensions.treemanifest= treemanifest.treeonly=1 remotefilelog.reponame=x
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: hgsubversion

  $ newrepo
  __del__ called
  $ setconfig extensions.hgsubversion=
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: sparse

  $ newrepo
  __del__ called
  $ setconfig extensions.sparse=
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: commitcloud

  $ newrepo
  __del__ called
  $ setconfig extensions.infinitepush= extensions.infinitepushbackup= extensions.commitcloud=
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Fine extension: sampling

  $ newrepo
  __del__ called
  $ setconfig extensions.sampling=
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

Somehow problematic: With many extensions

  $ newrepo
  __del__ called
  $ echo remotefilelog >> .hg/requires
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > absorb=
  > amend=
  > arcdiff=
  > automv=
  > blackbox=
  > checkmessagehook=
  > chistedit=
  > cleanobsstore=!
  > clienttelemetry=
  > clindex=
  > color=
  > commitcloud=
  > conflictinfo=
  > copytrace=
  > crdump=
  > debugcommitmessage=
  > dialect=
  > directaccess=
  > dirsync=
  > errorredirect=!
  > extorder=
  > extorder=
  > fastannotate=
  > fastlog=
  > fastmanifest=
  > fastpartialmatch=!
  > fbconduit=
  > fbhistedit=
  > githelp=
  > gitlookup=!
  > gitrevset=!
  > grpcheck=
  > hgevents=
  > hgsubversion=
  > hiddenerror=
  > histedit=
  > infinitepush=
  > infinitepushbackup=
  > journal=
  > logginghelper=
  > lz4revlog=
  > mergedriver =
  > mergedriver=
  > morecolors=
  > morestatus=
  > myparent=
  > patchrmdir=
  > phabdiff=
  > phabstatus=
  > phrevset=
  > progressfile=
  > pullcreatemarkers=
  > purge=
  > pushrebase =
  > pushrebase=
  > rage=
  > rebase =
  > rebase=
  > remotefilelog =
  > remotefilelog=
  > remotenames=
  > reset=
  > sampling=
  > shelve=
  > sigtrace=
  > simplecache=
  > smartlog=
  > sparse=
  > sshaskpass=
  > stat=
  > strip=
  > traceprof=
  > treedirstate=
  > treemanifest=
  > tweakdefaults=
  > undo=
  > 
  > [phases]
  > publish = False
  > 
  > [remotefilelog]
  > datapackversion = 1
  > fastdatapack = True
  > historypackv1 = True
  > reponame = x
  > cachepath = $TESTTMP/cache
  > 
  > [treemanifest]
  > treeonly=True
  > 
  > [fbconduit]
  > host=example.com
  > path=/conduit/
  > reponame=x
  > EOF
  $ hg log -r . -T '{manifest % "{node}"}\n'
  0000000000000000000000000000000000000000
  __del__ called

  $ touch x

 (this behaves differently with buck / setup.py build)

  $ hg ci -m x -A x
  __del__ called (?)

  $ hg log -r . -T '{manifest % "{node}"}\n'
  c2ffc254676c538a75532e7b6ebbbccaf98e2545
  __del__ called
