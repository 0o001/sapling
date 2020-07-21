#chg-compatible

Check that renames are correctly saved by a commit after a merge

Test with the merge on 3 having the rename on the local parent

  $ hg init a
  $ cd a

  $ echo line1 > foo
  $ hg add foo
  $ hg ci -m '0: add foo'

  $ echo line2 >> foo
  $ hg ci -m '1: change foo'

  $ hg up -C 0
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved

  $ hg mv foo bar
  $ rm bar
  $ echo line0 > bar
  $ echo line1 >> bar
  $ hg ci -m '2: mv foo bar; change bar'

  $ hg merge 1
  merging bar and foo to bar
  0 files updated, 1 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)

  $ cat bar
  line0
  line1
  line2

  $ hg ci -m '3: merge with local rename'

#if common-zlib
  $ hg debugindex bar
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0      77  .....       2 d35118874825 000000000000 000000000000 (re)
       1        77      80      0       3 5345f5ab8abd 000000000000 d35118874825
#endif

  $ hg debugrename bar
  bar renamed from foo:9e25c27b87571a1edee5ae4dddee5687746cc8e2

#if common-zlib
  $ hg debugindex foo
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0       7  .....       0 690b295714ae 000000000000 000000000000 (re)
       1         7      13  .....       1 9e25c27b8757 690b295714ae 000000000000 (re)
#endif

Revert the content change from rev 2:

  $ hg up -C 2
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ rm bar
  $ echo line1 > bar
  $ hg ci -m '4: revert content change from rev 2'

  $ hg log --template '{rev}:{node|short} {parents}\n'
  4:2263c1be0967 0f2ff26688b9 
  3:0555950ead28 0f2ff26688b9 5cd961e4045d 
  2:0f2ff26688b9 2665aaee66e9 
  1:5cd961e4045d 
  0:2665aaee66e9 

This should use bar@rev2 as the ancestor:

  $ hg --debug merge 3
    searching for copies back to rev 1
  resolving manifests
   branchmerge: True, force: False, partial: False
   ancestor: 0f2ff26688b9, local: 2263c1be0967+, remote: 0555950ead28
   preserving bar for resolve of bar
   bar: versions differ -> m (premerge)
  picked tool ':merge' for bar (binary False symlink False changedelete False)
  merging bar
  my bar@2263c1be0967+ other bar@0555950ead28 ancestor bar@0f2ff26688b9
   premerge successful
  0 files updated, 1 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)

  $ cat bar
  line1
  line2

  $ hg ci -m '5: merge'

#if common-zlib
  $ hg debugindex bar
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0      77  .....       2 d35118874825 000000000000 000000000000 (re)
       1        77      80      0       3 5345f5ab8abd 000000000000 d35118874825
       2       157       7     -1       4 ff4b45017382 d35118874825 000000000000
       3       164      13     -1       5 3701b4893544 ff4b45017382 5345f5ab8abd
#endif

Same thing, but with the merge on 3 having the rename
on the remote parent:

  $ cd ..
  $ hg clone -U -r 1 -r 2 a b
  adding changesets
  adding manifests
  adding file changes
  added 3 changesets with 3 changes to 2 files
  $ cd b

  $ hg up -C 1
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved

  $ hg merge 2
  merging foo and bar to bar
  0 files updated, 1 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)

  $ cat bar
  line0
  line1
  line2

  $ hg ci -m '3: merge with remote rename'

#if common-zlib
  $ hg debugindex bar
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0      77  .....       2 d35118874825 000000000000 000000000000 (re)
       1        77      80      0       3 5345f5ab8abd 000000000000 d35118874825
#endif

  $ hg debugrename bar
  bar renamed from foo:9e25c27b87571a1edee5ae4dddee5687746cc8e2

#if common-zlib
  $ hg debugindex foo
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0       7  .....       0 690b295714ae 000000000000 000000000000 (re)
       1         7      13  .....       1 9e25c27b8757 690b295714ae 000000000000 (re)
#endif

Revert the content change from rev 2:

  $ hg up -C 2
  1 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ rm bar
  $ echo line1 > bar
  $ hg ci -m '4: revert content change from rev 2'

  $ hg log --template '{rev}:{node|short} {parents}\n'
  4:2263c1be0967 0f2ff26688b9 
  3:3ffa6b9e35f0 5cd961e4045d 0f2ff26688b9 
  2:0f2ff26688b9 2665aaee66e9 
  1:5cd961e4045d 
  0:2665aaee66e9 

This should use bar@rev2 as the ancestor:

  $ hg --debug merge 3
    searching for copies back to rev 1
  resolving manifests
   branchmerge: True, force: False, partial: False
   ancestor: 0f2ff26688b9, local: 2263c1be0967+, remote: 3ffa6b9e35f0
   preserving bar for resolve of bar
   bar: versions differ -> m (premerge)
  picked tool ':merge' for bar (binary False symlink False changedelete False)
  merging bar
  my bar@2263c1be0967+ other bar@3ffa6b9e35f0 ancestor bar@0f2ff26688b9
   premerge successful
  0 files updated, 1 files merged, 0 files removed, 0 files unresolved
  (branch merge, don't forget to commit)

  $ cat bar
  line1
  line2

  $ hg ci -m '5: merge'

#if common-zlib
  $ hg debugindex bar
     rev    offset  length  ..... linkrev nodeid       p1           p2 (re)
       0         0      77  .....       2 d35118874825 000000000000 000000000000 (re)
       1        77      80      0       3 5345f5ab8abd 000000000000 d35118874825
       2       157       7     -1       4 ff4b45017382 d35118874825 000000000000
       3       164      13     -1       5 3701b4893544 ff4b45017382 5345f5ab8abd
#endif

  $ cd ..
