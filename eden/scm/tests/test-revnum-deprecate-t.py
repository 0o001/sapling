# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

from testutil.dott import feature, sh, testtmp  # noqa: F401


sh % "hg init"
(
    sh % "hg debugdrawdag"
    << r"""
C
|
B
|
A
"""
)

sh % "setconfig 'devel.legacy.revnum=warn'"

# use revnum directly

sh % "hg log -r 0 -T '.\\n'" == r"""
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 0) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

# negative revnum

sh % "hg update -r -2" == r"""
    2 files updated, 0 files merged, 0 files removed, 0 files unresolved
    hint[revnum-deprecate]: Local revision numbers (ex. -2) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

# revset operators

sh % "hg log -r 1+2 -T '.\\n'" == r"""
    .
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 1) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

sh % "hg log -r '::2' -T '.\\n'" == r"""
    .
    .
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 2) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

sh % "hg log -r 2-1 -T '.\\n'" == r"""
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 2) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

# revset functions

sh % "hg log -r 'parents(2)' -T '.\\n'" == r"""
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 2) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

sh % "hg log -r 'sort(2+0)' -T '.\\n'" == r"""
    .
    .
    hint[revnum-deprecate]: Local revision numbers (ex. 2) are being deprecated and will stop working in the future. Please use commit hashes instead.
    hint[hint-ack]: use 'hg hint --ack revnum-deprecate' to silence these hints"""

# abort

sh % "setconfig 'devel.legacy.revnum=abort'"
sh % "hg up 0" == r"""
    abort: local revision number is disabled in this repo
    [255]"""

# smartlog revset

sh % "enable smartlog"
sh % "hg log -r 'smartlog()' -T." == "..."
sh % "hg log -r 'smartlog(1)' -T." == r"""
    abort: local revision number is disabled in this repo
    [255]"""

# phase

sh % "hg phase" == "112478962961147124edd43549aedd1a335e44bf: draft"
