#debugruntest-compatible
# Copyright (c) Meta Platforms, Inc. and affiliates.
# Copyright (c) Mercurial Contributors.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

  $ setconfig devel.segmented-changelog-rev-compat=true
  $ setconfig workingcopy.ruststatus=false
  $ hg init test
  $ cd test
  $ echo a > changed
  $ echo a > removed
  $ echo a > source
  $ hg ci -Am addfiles
  adding changed
  adding removed
  adding source
  $ echo a >> changed
  $ echo a > added
  $ hg add added
  $ hg rm removed
  $ hg cp source copied
  $ hg diff --git > ../unknown.diff

# Test adding on top of an unknown file

  $ hg up -qC 0
  $ hg purge
  $ echo a > added
  $ hg import --no-commit ../unknown.diff
  applying ../unknown.diff
  file added already exists
  1 out of 1 hunks FAILED -- saving rejects to file added.rej
  abort: patch failed to apply
  [255]

# Test modifying an unknown file

  $ hg revert -aq
  $ hg purge
  $ hg rm changed
  $ hg ci -m removechanged
  $ echo a > changed
  $ hg import --no-commit ../unknown.diff
  applying ../unknown.diff
  abort: cannot patch changed: file is not tracked
  [255]

# Test removing an unknown file

  $ hg up -qC 0
  $ hg purge
  $ hg rm removed
  $ hg ci -m removeremoved
  $ echo a > removed
  $ hg import --no-commit ../unknown.diff
  applying ../unknown.diff
  abort: cannot patch removed: file is not tracked
  [255]

# Test copying onto an unknown file

  $ hg up -qC 0
  $ hg purge
  $ echo a > copied
  $ hg import --no-commit ../unknown.diff
  applying ../unknown.diff
  abort: cannot create copied: destination already exists
  [255]

  $ cd ..
