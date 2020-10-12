#require py2
#chg-compatible

#chg-compatible

#testcases case-innodb case-rocksdb
  $ disable treemanifest

#if case-rocksdb
  $ DBENGINE=rocksdb
#else
  $ DBENGINE=innodb
#endif

no-check-code

Python2 is required for its binary path handling

  $ hash python2 &>/dev/null || { echo 'skipped: missing python2'; exit 80; }
  $ . "$TESTDIR/hgsql/library.sh"

Create a repo with non-ascii paths

  $ initclient client1
  $ cd client1

..Create a file with tailing space (test the database will not eat the space)

  $ echo a > 'a    '
  $ if [ $? -ne 0 ]; then
  >   echo 'skipped: filesystem does not support binary paths'
  >   exit 80
  > fi
  $ hg add . -q 2>/dev/null
  $ hg commit -m 'tailing spaces' -q

..Create files with utf8 encoded unicode characters

  $ python2 2>/dev/null << EOF
  > for i in range(2, 256):
  >     if chr(i) in '/.\n\r':
  >         continue
  >     name = (unichr(i) * (i % 7 + 1)).encode('utf8')
  >     with open(name, 'wb') as f:
  >         f.write(b'foo')
  > EOF

  $ if [ $? -ne 0 ]; then
  >   echo 'skipped: filesystem does not support non ascii paths'
  >   exit 80
  > fi

  $ hg add . -q 2>/dev/null
  $ hg commit -m 'nonascii paths' -q

  $ hg log -T '{node} {desc}\n'
  a03fe2c5c0c917d545f5026260290c25e311871f nonascii paths
  c2d59fc1ca219a78013735473161145cb4d7d7fc tailing spaces

  $ hg bookmark nonasciipath
  $ cd ..

Create the master repo

  $ initserver master1 sqlreponame
  $ cd master1
  $ hg pull -q ../client1

  $ hg log -T '{node} {desc}\n'
  a03fe2c5c0c917d545f5026260290c25e311871f nonascii paths
  c2d59fc1ca219a78013735473161145cb4d7d7fc tailing spaces

  $ cd ..

Create another master repo, it should synchronize from the database

  $ initserver master2 sqlreponame
  $ cd master2
  $ hg log -T '{node} {desc}\n'
  a03fe2c5c0c917d545f5026260290c25e311871f nonascii paths
  c2d59fc1ca219a78013735473161145cb4d7d7fc tailing spaces

  $ hg bookmark
     nonasciipath              a03fe2c5c0c9

  $ hg up nonasciipath -q
  $ [[ -f 'a    ' ]] && echo good
  good

  $ hg verify
  checking changesets
  checking manifests
  crosschecking files in changesets and manifests
  checking files
  251 files, 2 changesets, 251 total revisions

  $ cd ..
