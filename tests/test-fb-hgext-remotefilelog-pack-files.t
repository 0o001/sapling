
  $ shorttraceback

  $ . "$TESTDIR/library.sh"

  $ hginit master
  $ cd master
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > server=True
  > serverexpiration=-1
  > EOF
  $ echo line > file
  $ echo x > x
  $ hg commit -qAm x
  $ echo line >> file
  $ echo x >> x
  $ hg commit -qAm x2
  $ findfilessorted .hg/store/data
  .hg/store/data/file.i
  .hg/store/data/x.i
  $ cd ..

  $ hgcloneshallow ssh://user@dummy/master shallow -q
  2 files fetched over 1 fetches - (2 misses, 0.00% hit ratio) over *s (glob)

# Set the prefetchdays config to zero so that all commits are prefetched
# no matter what their creation date is.
  $ cd shallow
  $ cat >> .hg/hgrc <<EOF
  > [remotefilelog]
  > localdatarepack=True
  > packlocaldata=True
  > prefetchdays=0
  > EOF
  $ cd ..

# Pull from master
  $ cd shallow
  $ hg pull -q
  $ hg up -q tip

# Test pack local data
  $ findfilessorted .hg/store/data
  $ test -d .hg/store/packs
  [1]

# new loose file is created
  $ echo "new commit" > new_file
  $ echo "something else" > base_file
  $ hg commit -qAm "one more node"
  $ findfilessorted .hg/store/data
  $ findfilessorted .hg/store/packs
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histidx
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histpack
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.dataidx
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.datapack

  $ echo "new commit - 2" > new_file
  $ hg commit -qAm "one more node - 2"
  $ findfilessorted .hg/store/data
  $ findfilessorted .hg/store/packs
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histidx
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histpack
  .hg/store/packs/3cd8d266014a45ac9f32ce6fe60bbba3ef841577.histidx
  .hg/store/packs/3cd8d266014a45ac9f32ce6fe60bbba3ef841577.histpack
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.dataidx
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.datapack
  .hg/store/packs/7a1982eca9ec13b2258dcf844adb3a536d733d75.dataidx
  .hg/store/packs/7a1982eca9ec13b2258dcf844adb3a536d733d75.datapack

# check the commit data
  $ hg cat -r . new_file
  new commit - 2
  $ hg cat -r .~1 new_file
  new commit

# Test repack
  $ hg repack --looseonly
  $ findfilessorted .hg/store/packs
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histidx
  .hg/store/packs/39a8e8bed95e2e3bb5391ee6bda40ca3ca572916.histpack
  .hg/store/packs/3cd8d266014a45ac9f32ce6fe60bbba3ef841577.histidx
  .hg/store/packs/3cd8d266014a45ac9f32ce6fe60bbba3ef841577.histpack
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.dataidx
  .hg/store/packs/6b9f244ddd95ecb2e7d32472c05bad1883b34cea.datapack
  .hg/store/packs/7a1982eca9ec13b2258dcf844adb3a536d733d75.dataidx
  .hg/store/packs/7a1982eca9ec13b2258dcf844adb3a536d733d75.datapack
  $ hg repack
  $ findfilessorted .hg/store/packs
  .hg/store/packs/7edafc4e8f1fcf89ce1abe2046a76ffff61d9e18.dataidx
  .hg/store/packs/7edafc4e8f1fcf89ce1abe2046a76ffff61d9e18.datapack
  .hg/store/packs/ba63ac333b14fd0bac5b28ffcf28c465a6d8e93a.histidx
  .hg/store/packs/ba63ac333b14fd0bac5b28ffcf28c465a6d8e93a.histpack

# check the commit data again
  $ hg cat -r . new_file
  new commit - 2
  $ hg cat -r .~1 new_file
  new commit
  $ hg up -r .~1
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ hg cat -r . base_file
  something else

# Check that files are not fetched from the server for the full history
# after clearing cache the file will be fetched because it needs a content of it
  $ findfilessorted $CACHEDIR
  $TESTTMP/hgcache/master/packs/729c1f9b99edec87840818cfda1cfb5c026549cd.dataidx
  $TESTTMP/hgcache/master/packs/729c1f9b99edec87840818cfda1cfb5c026549cd.datapack
  $TESTTMP/hgcache/master/packs/8439c9deb49aa426fddba0f12b66e39ed3b229f7.histidx
  $TESTTMP/hgcache/master/packs/8439c9deb49aa426fddba0f12b66e39ed3b229f7.histpack
  $TESTTMP/hgcache/master/packs/repacklock
  $TESTTMP/hgcache/repos
  $ clearcache
  $ echo "new line" >> file
  $ hg commit -vm "expecting to fetch"
  committing files:
  file
  committing manifest
  committing changelog
  committed changeset 4:dc68270aa18f
  calling hook commit.prefetch: hgext.remotefilelog.wcpprefetch
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob)

# if the loose file format is used then the file will be fetched because of the
# historical data the content is known, because previous cahnges were local
  $ clearcache
  $ echo "new line" >> file
  $ hg commit -vm "check still fetches" --config "remotefilelog.packlocaldata=False"
  committing files:
  file
  committing manifest
  committing changelog
  committed changeset 5:74c424ec1e23
  calling hook commit.prefetch: hgext.remotefilelog.wcpprefetch
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over *s (glob)
  $ findfilessorted $CACHEDIR
  $TESTTMP/hgcache/master/97/1c419dd609331343dee105fffd0f4608dc0bf2/ea096176809b81541cdb77bc9dcf6a43a7ea6bc7
  $TESTTMP/hgcache/master/97/1c419dd609331343dee105fffd0f4608dc0bf2/filename
  $TESTTMP/hgcache/repos

# don't need fetch anything if the pack files format is used
  $ clearcache
  $ echo "new line" >> file
  $ hg commit -vm "won't download"
  committing files:
  file
  committing manifest
  committing changelog
  committed changeset 6:3462713eae99
  calling hook commit.prefetch: hgext.remotefilelog.wcpprefetch
  $ findfilessorted $CACHEDIR


# Test that it doesn't break non-remotefilelog repos

  $ newrepo
  $ setconfig remotefilelog.packlocaldata=True
  $ echo 1 >> a
  $ hg commit -Am "commit"
  adding a

# Test moving files get correctly written into the pack
  $ cd ../shallow
  $ hg mv x xx
  $ hg commit -m "move x to xx"
  $ hg debugfilerev -r .
  e61a2a5ac410: move x to xx
   xx: bin=0 lnk=0 flag=0 size=4 copied='x' chain=ede4cf65837b
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over 0.00s
  $ hg debugdatapack --node ede4cf65837b184e3aea21fb13ba08bcc26f7988 .hg/store/packs/*.dataidx
  .hg/store/packs/30e5a4f1fde0aa10de7c8bbc818d97aac0d4087e:
  (not found)
  .hg/store/packs/35f801efb2af8c02fe003127b71ce4b2083a20e1:
  (not found)
  .hg/store/packs/7edafc4e8f1fcf89ce1abe2046a76ffff61d9e18:
  (not found)
  .hg/store/packs/c5539d8d2a5eb7fc3b412e76a12bcd882f797e58:
  \x01 (esc)
  copy: x
  copyrev: aee31534993a501858fb6dd96a065671922e7d51
  \x01 (esc)
  x
  x

# Test that pending pack writes can be read
  $ cd ..
  $ hgcloneshallow ssh://user@dummy/master pending_test -q
  1 files fetched over 1 fetches - (1 misses, 0.00% hit ratio) over 0.00s
  $ cd pending_test
  $ setconfig rebase.singeltransaction=True remotefilelog.packlocaldata=True
  $ drawdag <<'EOS'
  > B # B/foo=x\nb\nc\nd\ne\n
  > |
  > | D # D/foo=a\nb\nc\nd\ne\nf\ng\n
  > | |
  > | C # C/foo=a\nb\nc\nd\ne\nf\n
  > |/
  > A # A/foo=a\nb\nc\nd\ne\n
  > EOS

- Rebase the stack of two onto the lone commit and force a file merge.
- This means both commits will be applied in the same transaction and the second
- commit will be required to read the new content of the first commit, which
- hasn't been flushed to disk yet.
  $ hg rebase -s $C -d $B
  rebasing 4:e6011cdb8530 "C"
  merging foo
  rebasing 5:f579bc04d2ae "D" (tip)
  merging foo

# Test compatibility with LFS

  $ newrepo
  $ echo remotefilelog >> .hg/requires
  $ enable lfs remotefilelog
  $ setconfig lfs.threshold=1B lfs.url=file://$TESTTMP/lfs remotefilelog.packlocaldata=1
  $ hg log
  $ printf THIS-IS-LFS > A
  $ hg ci -m A -A A
  $ drawdag <<'EOS'
  > A # A/A=THIS-IS-LFS
  > EOS
  $ hg cat -r $A A
  THIS-IS-LFS (no-eol)

  $ hg debugfilerev -r $A
  267e8be81b24: A
   A: bin=0 lnk=0 flag=2000 size=11 copied='' chain=7d8ad6217b8a
