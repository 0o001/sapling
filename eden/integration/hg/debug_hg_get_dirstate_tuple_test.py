#!/usr/bin/env python3
# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from textwrap import dedent

from eden.integration.lib import hgrepo

from .lib.hg_extension_test_base import EdenHgTestCase, hg_test


@hg_test
# pyre-ignore[13]: T62487924
class DebugHgGetDirstateTupleTest(EdenHgTestCase):
    def populate_backing_repo(self, repo: hgrepo.HgRepository) -> None:
        repo.write_file("hello", "hola\n")
        repo.write_file("dir/file", "blah\n")
        repo.commit("Initial commit.")

    def test_get_dirstate_tuple_normal_file(self) -> None:
        output = self.eden.run_cmd(
            "debug", "hg_get_dirstate_tuple", self.get_path("hello")
        )
        expected = dedent(
            """\
        hello
            status = Normal
            mode = 0o100644
            mergeState = NotApplicable
        """
        )
        self.assertEqual(expected, output)
