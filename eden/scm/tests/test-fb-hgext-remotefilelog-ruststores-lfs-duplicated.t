#chg-compatible

  $ newserver master
  $ setconfig extensions.lfs= lfs.url=file:$TESTTMP/lfs-server

  $ clone master shallow --noupdate
  $ switchrepo shallow
  $ setconfig extensions.lfs= lfs.url=file:$TESTTMP/lfs-server lfs.threshold=10B

  $ echo "THIS IS AN LFS BLOB" > x
  $ hg commit -qAm x

# Copy the packfiles that contain LFS pointers before they get removed by the following repack.
  $ cp .hg/store/packs/*.data{pack,idx} $TESTTMP
  $ setconfig remotefilelog.lfs=True remotefilelog.localdatarepack=True
  $ setconfig remotefilelog.maintenance.timestamp.localrepack=1 remotefilelog.maintenance=localrepack
  $ hg repack
  Running a one-time local repack, this may take some time
  Done with one-time local repack

# Copy back the packfiles. We now have a filenode with pointer in 2 different location, the packfile, and the lfs store.
  $ cp "$TESTTMP/"*.data{pack,idx} .hg/store/packs

# Make sure that bundle isn't confused by this.
  $ hg bundle -q -r . $TESTTMP/test-bundle

  $ clone master shallow2 --noupdate
  $ switchrepo shallow2
  $ setconfig remotefilelog.lfs=True lfs.url=file:$TESTTMP/lfs-server lfs.threshold=10GB

  $ hg unbundle -q -u $TESTTMP/test-bundle
  $ cat x
  THIS IS AN LFS BLOB
