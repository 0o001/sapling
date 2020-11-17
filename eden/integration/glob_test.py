#!/usr/bin/env python3
# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from typing import List, Optional

from facebook.eden.ttypes import EdenError, EdenErrorType, GlobParams

from .lib import testcase


@testcase.eden_repo_test
class GlobTest(testcase.EdenRepoTest):
    def populate_repo(self) -> None:
        self.repo.write_file("hello", "bonjour\n")
        self.repo.write_file("hola", "hello\n")
        self.repo.write_file("adir/phile", "phoo!\n")

        self.commit0 = self.repo.commit("Commit 0.")

        self.repo.remove_files(["hello", "hola", "adir/phile"])
        self.repo.write_file("hello", "hola\n")
        self.repo.write_file("adir/file", "foo!\n")
        self.repo.write_file("bdir/file", "bar!\n")
        self.repo.write_file("bdir/otherfile", "foo!\n")
        self.repo.symlink("slink", "hello")
        self.repo.write_file("cdir/subdir/new.txt", "and improved")
        self.repo.write_file("ddir/notdotfile", "")
        self.repo.write_file("ddir/subdir/notdotfile", "")
        self.repo.write_file("ddir/subdir/.dotfile", "")

        self.repo.write_file("java/com/example/package.html", "")
        self.repo.write_file("java/com/example/Example.java", "")
        self.repo.write_file("java/com/example/foo/Foo.java", "")
        self.repo.write_file("java/com/example/foo/bar/Bar.java", "")
        self.repo.write_file("java/com/example/foo/bar/baz/Baz.java", "")

        self.commit1 = self.repo.commit("Commit 1.")

    def setUp(self) -> None:
        # needs to be done before set up because these need to be created
        # for populate_repo() and the supers set up will call this.
        self.commit0 = ""
        self.commit1 = ""

        super().setUp()

        self.client = self.get_thrift_client()
        self.client.open()
        self.addCleanup(self.client.close)

    def test_exact_path_component_match(self) -> None:
        self.assert_glob(["hello"], [b"hello"])
        self.assert_glob(["ddir/subdir/.dotfile"], [b"ddir/subdir/.dotfile"])

    def test_wildcard_path_component_match(self) -> None:
        self.assert_glob(["hel*"], [b"hello"])
        self.assert_glob(["ad*"], [b"adir"])
        self.assert_glob(["a*/file"], [b"adir/file"])

    def test_no_accidental_substring_match(self) -> None:
        self.assert_glob(["hell"], [], msg="No accidental substring match")

    def test_match_all_files_in_directory(self) -> None:
        self.assert_glob(["bdir/*"], [b"bdir/file", b"bdir/otherfile"])

    def test_match_all_files_in_directory_with_dotfile(self) -> None:
        self.assert_glob(["ddir/subdir/*"], [b"ddir/subdir/notdotfile"])

    def test_overlapping_globs(self) -> None:
        self.assert_glob(
            ["adir/*", "**/file"],
            [b"adir/file", b"bdir/file"],
            msg="De-duplicate results from multiple globs",
        )

    def test_recursive_wildcard_prefix(self) -> None:
        self.assert_glob(["**/file"], [b"adir/file", b"bdir/file"])

    def test_recursive_wildcard_suffix(self) -> None:
        self.assert_glob(["adir/**"], [b"adir/file"])
        self.assert_glob(["adir/**/*"], [b"adir/file"])

    def test_recursive_wildcard_suffix_with_dotfile(self) -> None:
        self.assert_glob(
            ["ddir/**"], [b"ddir/notdotfile", b"ddir/subdir", b"ddir/subdir/notdotfile"]
        )
        self.assert_glob(
            ["ddir/**"],
            [
                b"ddir/subdir",
                b"ddir/subdir/.dotfile",
                b"ddir/notdotfile",
                b"ddir/subdir/notdotfile",
            ],
            include_dotfiles=True,
        )

        self.assert_glob(
            ["ddir/**/*"],
            [b"ddir/notdotfile", b"ddir/subdir", b"ddir/subdir/notdotfile"],
        )
        self.assert_glob(
            ["ddir/**/*"],
            [
                b"ddir/subdir",
                b"ddir/subdir/.dotfile",
                b"ddir/notdotfile",
                b"ddir/subdir/notdotfile",
            ],
            include_dotfiles=True,
        )

    def test_qualified_recursive_wildcard(self) -> None:
        self.assert_glob(
            ["java/com/**/*.java"],
            [
                b"java/com/example/Example.java",
                b"java/com/example/foo/Foo.java",
                b"java/com/example/foo/bar/Bar.java",
                b"java/com/example/foo/bar/baz/Baz.java",
            ],
        )
        self.assert_glob(
            ["java/com/example/*/*.java"], [b"java/com/example/foo/Foo.java"]
        )

    def test_malformed_query(self) -> None:
        with self.assertRaises(EdenError) as ctx:
            self.client.glob(self.mount_path_bytes, ["adir["])
        self.assertIn("unterminated bracket sequence", str(ctx.exception))
        self.assertEqual(EdenErrorType.POSIX_ERROR, ctx.exception.errorType)

        with self.assertRaises(EdenError) as ctx:
            self.client.globFiles(GlobParams(self.mount_path_bytes, ["adir["], True))
        self.assertIn("unterminated bracket sequence", str(ctx.exception))
        self.assertEqual(EdenErrorType.POSIX_ERROR, ctx.exception.errorType)

    def test_globs_may_not_include_dotdot(self):
        with self.assertRaises(EdenError) as ctx:
            self.client.globFiles(
                GlobParams(self.mount_path_bytes, ["java/../java/com/**/*.java"])
            )
        self.assertEqual(
            "Invalid glob (PathComponent must not be ..): java/../java/com/**/*.java",
            str(ctx.exception),
        )
        self.assertEqual(EdenErrorType.ARGUMENT_ERROR, ctx.exception.errorType)

    def test_glob_on_non_current_commit(self) -> None:
        self.assert_glob(["hello"], [b"hello"], commits=[bytes.fromhex(self.commit0)])
        self.assert_glob(["hola"], [b"hola"], commits=[bytes.fromhex(self.commit0)])

    def test_glob_multiple_commits(self) -> None:
        self.assert_glob(
            ["hello"],
            [b"hello", b"hello"],
            commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
        )
        self.assert_glob(
            ["h*"],
            [b"hello", b"hello", b"hola"],
            commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
        )
        self.assert_glob(
            ["a*/*ile"],
            [b"adir/file", b"adir/phile"],
            commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
        )

    def test_prefetch_matching_files(self) -> None:
        self.assert_glob(["hello"], [b"hello"], prefetching=True)
        self.assert_glob(
            ["hello"],
            [b"hello"],
            prefetching=True,
            commits=[bytes.fromhex(self.commit0)],
        )
        self.assert_glob(
            ["hello"],
            [b"hello", b"hello"],
            prefetching=True,
            commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
        )

    def test_simple_matching_commit(self) -> None:
        self.assert_glob(
            ["hello"],
            expected_matches=[b"hello"],
            expected_commits=[bytes.fromhex(self.commit1)],
        )

        self.assert_glob(
            ["hello"],
            expected_matches=[b"hello"],
            expected_commits=[bytes.fromhex(self.commit0)],
            commits=[bytes.fromhex(self.commit0)],
        )

    def test_duplicate_file_multiple_commits(self) -> None:
        self.assert_glob(
            ["hello"],
            expected_matches=[b"hello", b"hello"],
            expected_commits=[
                bytes.fromhex(self.commit0),
                bytes.fromhex(self.commit1),
            ],
            commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
        )

        def test_multiple_file_multiple_commits(self) -> None:
            self.assert_glob(
                ["a*/*ile"],
                [b"adir/file", b"adir/phile"],
                expected_commits=[
                    bytes.fromhex(self.commit1),
                    bytes.fromhex(self.commit0),
                ],
                commits=[bytes.fromhex(self.commit0), bytes.fromhex(self.commit1)],
            )

    def assert_glob(
        self,
        globs: List[str],
        expected_matches: List[bytes],
        include_dotfiles: bool = False,
        msg: Optional[str] = None,
        commits: Optional[List[bytes]] = None,
        prefetching: bool = False,
        expected_commits: Optional[List[bytes]] = None,
    ) -> None:
        params = GlobParams(
            mountPoint=self.mount_path_bytes,
            globs=globs,
            includeDotfiles=include_dotfiles,
            prefetchFiles=prefetching,
            revisions=commits,
        )
        self.assertCountEqual(
            expected_matches, self.client.globFiles(params).matchingFiles, msg=msg
        )

        if expected_commits:
            self.assertCountEqual(
                expected_commits, self.client.globFiles(params).originHashes, msg=msg
            )

        # Also verify behavior of legacy Thrift API.
        if include_dotfiles:
            self.assertCountEqual(
                expected_matches,
                self.client.glob(self.mount_path_bytes, globs),
                msg=msg,
            )
