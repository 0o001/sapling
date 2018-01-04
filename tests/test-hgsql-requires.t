  $ . "$TESTDIR/hgsql/library.sh"

# Populate the db with an initial commit

  $ initclient client
  $ cd client
  $ echo x > x
  $ hg commit -qAm x
  $ cd ..

  $ initserver master masterrepo
  $ cd master
  $ hg log
  $ hg pull -q ../client

Test that hgsql is a requirement
  $ grep hgsql .hg/requires
  hgsql
  $ hg log -r tip --config extensions.hgsql=!
  abort: repository requires features unknown to this Mercurial: hgsql!
  (see https://mercurial-scm.org/wiki/MissingRequirement for more information)
  [255]
  $ hg log -r tip
  changeset:   0:b292c1e3311f
  tag:         tip
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  summary:     x
  

Ensure streaming clones to non-hgsql repos work
  $ cd ..
  $ hg clone --config extensions.hgsql=! --config ui.ssh='python "$TESTDIR/dummyssh"' --uncompressed ssh://user@dummy/master client2 | grep "streaming all changes"
  streaming all changes

