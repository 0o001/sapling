  $ extpath=`dirname $TESTDIR`
  $ cp $extpath/hgext3rd/hiddenerror.py $TESTTMP
  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > hiddenerror=$TESTTMP/hiddenerror.py
  > [experimental]
  > evolution=all
  > EOF

Create hidden changeset.
  $ hg init repo && cd repo
  $ hg debugbuilddag +1
  $ hg debugobsolete 1ea73414a91b0920940797d8fc6a11e447f8ea1e

Test default error message.
  $ hg log -r 0
  abort: hidden changeset 1ea73414a91b!
  [255]

Test custom error message without hash.
  $ cat >> $HGRCPATH << EOF
  > [hiddenerror]
  > message = message without hash
  > hint = hint without hash
  > EOF
  $ hg log -r 0
  abort: message without hash!
  (hint without hash)
  [255]

Test custom error message with hash.
  $ cat >> $HGRCPATH << EOF
  > [hiddenerror]
  > message = message with hash {0}
  > hint = hint with hash {0}
  > EOF
  $ hg log -r 0
  abort: message with hash 1ea73414a91b!
  (hint with hash 1ea73414a91b)
  [255]

Test that basic operations like `status` don't throw an exception due
to the wrapped context constructor.
  $ hg status
