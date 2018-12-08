#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import os
import pathlib
import subprocess
import sys
import typing
import unittest
from typing import List, Optional

import pexpect
from eden.cli.config import EdenInstance
from eden.cli.util import HealthStatus
from eden.test_support.temporary_directory import TemporaryDirectoryMixin
from fb303.ttypes import fb_status

from .lib import testcase
from .lib.edenfs_systemd import EdenFSSystemdMixin
from .lib.environment_variable import EnvironmentVariableMixin
from .lib.fake_edenfs import read_fake_edenfs_argv_file
from .lib.find_executables import FindExe
from .lib.pexpect import PexpectAssertionMixin, wait_for_pexpect_process
from .lib.service_test_case import ServiceTestCaseBase, service_test
from .lib.systemd import SystemdUserServiceManagerMixin


class StartTest(testcase.EdenTestCase):
    def test_start_if_necessary(self) -> None:
        # Confirm there are no checkouts configured, then stop edenfs
        checkouts = self.eden.list_cmd()
        self.assertEqual({}, checkouts)
        self.assertTrue(self.eden.is_healthy())
        self.eden.shutdown()
        self.assertFalse(self.eden.is_healthy())

        # `eden start --if-necessary` should not start eden
        output = self.eden.run_cmd("start", "--if-necessary")
        self.assertEqual("No Eden mount points configured.\n", output)
        self.assertFalse(self.eden.is_healthy())

        # Restart eden and create a checkout
        self.eden.start()
        self.assertTrue(self.eden.is_healthy())

        # Create a repository with one commit
        repo = self.create_hg_repo("testrepo")
        repo.write_file("README", "test\n")
        repo.commit("Initial commit.")
        # Create an Eden checkout of this repository
        checkout_dir = os.path.join(self.mounts_dir, "test_checkout")
        self.eden.clone(repo.path, checkout_dir)

        checkouts = self.eden.list_cmd()
        self.assertEqual({checkout_dir: self.eden.CLIENT_ACTIVE}, checkouts)

        # Stop edenfs
        self.eden.shutdown()
        self.assertFalse(self.eden.is_healthy())
        # `eden start --if-necessary` should start edenfs now
        if eden_start_needs_allow_root_option(systemd=False):
            output = self.eden.run_cmd(
                "start", "--if-necessary", "--", "--allowRoot", capture_stderr=True
            )
        else:
            output = self.eden.run_cmd("start", "--if-necessary", capture_stderr=True)
        self.assertIn("Started edenfs", output)
        self.assertTrue(self.eden.is_healthy())

        # Stop edenfs.  We didn't start it through self.eden.start()
        # so the self.eden class doesn't really know it is running and that
        # it needs to be shut down.
        self.eden.run_cmd("stop")


@testcase.eden_repo_test
class StartWithRepoTest(
    testcase.EdenRepoTest,
    EnvironmentVariableMixin,
    SystemdUserServiceManagerMixin,
    EdenFSSystemdMixin,
):
    """Test 'eden start' with a repo and checkout already configured.
    """

    def setUp(self) -> None:
        super().setUp()
        self.eden.shutdown()

    def test_eden_start_mounts_checkouts(self) -> None:
        self.run_eden_start(systemd=False)
        self.assert_checkout_is_mounted()

    def test_eden_start_with_systemd_mounts_checkouts(self) -> None:
        self.set_up_edenfs_systemd_service()
        self.run_eden_start(systemd=True)
        self.assert_checkout_is_mounted()

    def run_eden_start(self, systemd: bool) -> None:
        env = dict(os.environ)
        if systemd:
            env["EDEN_EXPERIMENTAL_SYSTEMD"] = "1"
        else:
            env.pop("EDEN_EXPERIMENTAL_SYSTEMD", None)
        command = [
            FindExe.EDEN_CLI,
            "--config-dir",
            self.eden_dir,
            "start",
            "--daemon-binary",
            FindExe.EDEN_DAEMON,
        ]
        if eden_start_needs_allow_root_option(systemd=systemd):
            command.extend(["--", "--allowRoot"])
        subprocess.check_call(command, env=env)

    def assert_checkout_is_mounted(self) -> None:
        file = pathlib.Path(self.mount) / "hello"
        self.assertTrue(file.is_file())
        self.assertEqual(file.read_text(), "hola\n")

    def populate_repo(self) -> None:
        self.repo.write_file("hello", "hola\n")
        self.repo.commit("Initial commit.")

    def select_storage_engine(self) -> str:
        """ we need to persist data across restarts """
        return "rocksdb"


class DirectInvokeTest(unittest.TestCase):
    def test_no_args(self) -> None:
        """Directly invoking edenfs with no arguments should fail."""
        self._check_error([])

    def test_eden_cmd_arg(self) -> None:
        """Directly invoking edenfs with an eden command should fail."""
        self._check_error(["restart"])

    def _check_error(self, args: List[str], err: Optional[str] = None) -> None:
        cmd = [FindExe.EDEN_DAEMON]
        cmd.extend(args)
        out = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(os.EX_USAGE, out.returncode)
        self.assertEqual(b"", out.stdout)

        if err is None:
            err = """\
error: the edenfs daemon should not normally be invoked manually
Did you mean to run "eden" instead of "edenfs"?
"""
        self.maxDiff = 5000
        self.assertMultiLineEqual(err, out.stderr.decode("utf-8", errors="replace"))


@service_test
class StartFakeEdenFSTest(
    ServiceTestCaseBase, PexpectAssertionMixin, TemporaryDirectoryMixin
):
    def setUp(self) -> None:
        super().setUp()
        self.eden_dir = pathlib.Path(self.make_temporary_directory())

    def test_eden_start_launches_separate_processes_for_separate_eden_dirs(
        self
    ) -> None:
        eden_dir_1 = self.eden_dir
        eden_dir_2 = pathlib.Path(self.make_temporary_directory())

        start_1_process = self.spawn_start(eden_dir=eden_dir_1)
        self.assert_process_succeeds(start_1_process)
        start_2_process = self.spawn_start(eden_dir=eden_dir_2)
        self.assert_process_succeeds(start_2_process)

        instance_1_health: HealthStatus = EdenInstance(
            str(eden_dir_1), etc_eden_dir=None, home_dir=None
        ).check_health()
        self.assertEqual(
            instance_1_health.status,
            fb_status.ALIVE,
            f"First edenfs process should be healthy, but it isn't: "
            f"{instance_1_health}",
        )

        instance_2_health: HealthStatus = EdenInstance(
            str(eden_dir_2), etc_eden_dir=None, home_dir=None
        ).check_health()
        self.assertEqual(
            instance_2_health.status,
            fb_status.ALIVE,
            f"Second edenfs process should be healthy, but it isn't: "
            f"{instance_2_health}",
        )

        self.assertNotEqual(
            instance_1_health.pid,
            instance_2_health.pid,
            f"The edenfs process should have separate process IDs",
        )

    def test_daemon_command_arguments_should_forward_to_edenfs(self) -> None:
        argv_file = self.eden_dir / "argv"
        assert not argv_file.exists()

        extra_daemon_args = [
            "--commandArgumentsLogFile",
            str(argv_file),
            "--",
            "hello world",
            "--ignoredOption",
        ]
        start_process = self.spawn_start(daemon_args=extra_daemon_args)
        wait_for_pexpect_process(start_process)

        argv = read_fake_edenfs_argv_file(argv_file)
        self.assertEquals(
            argv[-len(extra_daemon_args) :],
            extra_daemon_args,
            f"fake_edenfs should have received arguments verbatim\nargv: {argv}",
        )

    def test_daemon_command_arguments_should_forward_to_edenfs_without_leading_dashdash(
        self
    ) -> None:
        argv_file = self.eden_dir / "argv"
        assert not argv_file.exists()

        subprocess.check_call(
            [
                FindExe.EDEN_CLI,
                "--config-dir",
                str(self.eden_dir),
                "start",
                "--daemon-binary",
                FindExe.FAKE_EDENFS,
                "hello world",
                "another fake_edenfs argument",
                "--",
                "--commandArgumentsLogFile",
                str(argv_file),
                "arg_after_dashdash",
            ]
        )

        expected_extra_daemon_args = [
            "hello world",
            "another fake_edenfs argument",
            "--commandArgumentsLogFile",
            str(argv_file),
            "arg_after_dashdash",
        ]
        argv = read_fake_edenfs_argv_file(argv_file)
        self.assertEquals(
            argv[-len(expected_extra_daemon_args) :],
            expected_extra_daemon_args,
            f"fake_edenfs should have received extra arguments\nargv: {argv}",
        )

    def test_eden_start_fails_if_edenfs_is_already_running(self) -> None:
        self.skip_if_systemd("TODO(T33122320)")
        with self.spawn_fake_edenfs(self.eden_dir) as daemon_pid:
            start_process = self.spawn_start()
            start_process.expect_exact(f"edenfs is already running (pid {daemon_pid})")
            self.assert_process_fails(start_process, 1)

    def test_eden_start_fails_if_edenfs_fails_during_startup(self) -> None:
        self.skip_if_systemd("TODO(T33122320): Forward startup logs to CLI")
        start_process = self.spawn_start(daemon_args=["--failDuringStartup"])
        start_process.expect_exact(
            "Started successfully, but reporting failure because "
            "--failDuringStartup was specified"
        )
        self.assert_process_fails(start_process, 1)

    def spawn_start(
        self,
        daemon_args: typing.Sequence[str] = (),
        eden_dir: typing.Optional[pathlib.Path] = None,
    ) -> "pexpect.spawn[str]":
        if eden_dir is None:
            eden_dir = self.eden_dir
        args = [
            "--config-dir",
            str(eden_dir),
            "start",
            "--daemon-binary",
            FindExe.FAKE_EDENFS,
        ]
        if daemon_args:
            args.append("--")
            args.extend(daemon_args)
        return pexpect.spawn(
            FindExe.EDEN_CLI, args, encoding="utf-8", logfile=sys.stderr
        )

    def __read_argv_file(self, argv_file: pathlib.Path) -> typing.List[str]:
        self.assertTrue(
            argv_file.exists(),
            f"fake_edenfs should have recognized the --commandArgumentsLogFile argument",
        )
        return list(argv_file.read_text().splitlines())


def eden_start_needs_allow_root_option(systemd: bool) -> bool:
    return not systemd and "SANDCASTLE" in os.environ
