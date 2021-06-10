#chg-compatible

  $ disable treemanifest
  $ hg init test
  $ cd test
  $ hg unbundle "$TESTDIR/bundles/remote.hg"
  adding changesets
  adding manifests
  adding file changes
  added 9 changesets with 7 changes to 4 files
  $ hg up tip
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd ..

  $ for i in 0 1 2 3 4 5 6 7 8; do
  >    mkdir test-"$i"
  >    hg --cwd test-"$i" init
  >    hg -R test bundle -r "$i" test-"$i".hg test-"$i"
  >    cd test-"$i"
  >    hg unbundle ../test-"$i".hg
  >    hg verify
  >    hg tip -q
  >    cd ..
  > done
  searching for changes
  1 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 1 changes to 1 files
  warning: verify does not actually check anything in this repo
  bfaf4b5cbf01
  searching for changes
  2 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 2 changes to 1 files
  warning: verify does not actually check anything in this repo
  21f32785131f
  searching for changes
  3 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 3 changesets with 3 changes to 1 files
  warning: verify does not actually check anything in this repo
  4ce51a113780
  searching for changes
  4 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 4 changesets with 4 changes to 1 files
  warning: verify does not actually check anything in this repo
  93ee6ab32777
  searching for changes
  2 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 2 changesets with 2 changes to 1 files
  warning: verify does not actually check anything in this repo
  c70afb1ee985
  searching for changes
  3 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 3 changesets with 3 changes to 1 files
  warning: verify does not actually check anything in this repo
  f03ae5a9b979
  searching for changes
  4 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 4 changesets with 5 changes to 2 files
  warning: verify does not actually check anything in this repo
  095cb14b1b4d
  searching for changes
  5 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 5 changesets with 6 changes to 3 files
  warning: verify does not actually check anything in this repo
  faa2e4234c7a
  searching for changes
  5 changesets found
  adding changesets
  adding manifests
  adding file changes
  added 5 changesets with 5 changes to 2 files
  warning: verify does not actually check anything in this repo
  916f1afdef90
  $ cd test-8
  $ hg pull ../test-7
  pulling from ../test-7
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 4 changesets with 2 changes to 3 files
  $ hg verify
  warning: verify does not actually check anything in this repo
  $ cd ..

should fail

  $ hg -R test bundle --base 2 -r tip test-bundle-branch1.hg test-3
  abort: --base is incompatible with specifying a destination
  [255]
  $ hg -R test bundle -a -r tip test-bundle-branch1.hg test-3
  abort: --all is incompatible with specifying a destination
  [255]
  $ hg -R test bundle -r tip test-bundle-branch1.hg
  abort: repository default-push not found!
  [255]

  $ hg -R test bundle --base 2 -r tip test-bundle-branch1.hg
  2 changesets found
  $ hg -R test bundle --base 2 -r 7 test-bundle-branch2.hg
  4 changesets found
  $ hg -R test bundle --base 2 test-bundle-all.hg
  6 changesets found
  $ hg -R test bundle --base 2 --all test-bundle-all-2.hg
  ignoring --base because --all was specified
  9 changesets found
  $ hg -R test bundle --base 3 -r tip test-bundle-should-fail.hg
  1 changesets found

empty bundle

  $ hg -R test bundle --base 7 --base 8 test-bundle-empty.hg
  no changes found
  [1]

issue76 msg2163

  $ hg -R test bundle --base 3 -r 3 -r 3 test-bundle-cset-3.hg
  no changes found
  [1]

Issue1910: 'hg bundle --base $head' does not exclude $head from
result

  $ hg -R test bundle --base 7 test-bundle-cset-7.hg
  4 changesets found

  $ hg clone test-2 test-9
  updating to branch default
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd test-9

revision 2

  $ hg tip -q
  4ce51a113780
  $ hg unbundle ../test-bundle-should-fail.hg
  adding changesets
  abort: 93ee6ab32777cd430e07da694794fb6a4f917712 cannot be found!
  [255]

revision 2

  $ hg tip -q
  4ce51a113780
  $ hg unbundle ../test-bundle-all.hg
  adding changesets
  adding manifests
  adding file changes
  added 6 changesets with 4 changes to 4 files

revision 8

  $ hg tip -q
  916f1afdef90
  $ hg verify
  warning: verify does not actually check anything in this repo

revision 2

  $ hg unbundle ../test-bundle-branch1.hg
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 2 files

revision 4

  $ hg verify
  warning: verify does not actually check anything in this repo
  $ hg unbundle ../test-bundle-branch2.hg
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 3 files

revision 6

  $ hg verify
  warning: verify does not actually check anything in this repo
  $ hg unbundle ../test-bundle-cset-7.hg
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 2 files

revision 4

  $ hg tip -q
  916f1afdef90
  $ hg verify
  warning: verify does not actually check anything in this repo

  $ cd ../test
  $ hg merge 7
  note: possible conflict - afile was renamed multiple times to:
   anotherfile
   adifferentfile
  2 files updated, 0 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)
  $ hg ci -m merge
  $ cd ..
  $ hg -R test bundle --base 2 test-bundle-head.hg
  7 changesets found
  $ hg clone test-2 test-10
  updating to branch default
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd test-10
  $ hg unbundle ../test-bundle-head.hg
  adding changesets
  adding manifests
  adding file changes
  added 7 changesets with 4 changes to 4 files

revision 9

  $ hg tip -q
  03fc0b0e347c
  $ hg verify
  warning: verify does not actually check anything in this repo

  $ cd ..
