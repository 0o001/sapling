#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import abc
import contextlib
import errno
import logging
import os
import os.path
import pathlib
import re
import subprocess
import sys
import tempfile
import threading
import types
import typing

from eden.cli.util import poll_until

from .find_executables import FindExe
from .linux import LinuxCgroup, ProcessID
from .temporary_directory import create_tmp_dir


logger = logging.getLogger(__name__)

SystemdUnitName = str


class SystemdUserServiceManager:
    """A running 'systemd --user' process manageable using 'systemctl --user'."""

    def __init__(
        self, xdg_runtime_dir: pathlib.Path, process_id: typing.Optional[ProcessID]
    ) -> None:
        super().__init__()
        self.__xdg_runtime_dir = xdg_runtime_dir
        self.__process_id = process_id

    @property
    def xdg_runtime_dir(self) -> pathlib.Path:
        return self.__xdg_runtime_dir

    @property
    def process_id(self) -> ProcessID:
        if self.__process_id is None:
            raise NotImplementedError()
        return self.__process_id

    def is_alive(self) -> bool:
        result = self._systemctl.run(
            ["--user", "show-environment"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        if result.returncode == 0:
            return True
        if result.returncode == 1:
            logger.warning(f'{self} is not alive: {result.stdout.decode("utf-8")}')
            return False
        result.check_returncode()
        return False

    def enable_runtime_unit_from_file(self, unit_file: pathlib.Path) -> None:
        self._systemctl.check_call(["enable", "--runtime", "--", unit_file])
        self._systemctl.check_call(["daemon-reload"])
        self.sanity_check_enabled_unit(unit_file=unit_file)

    def sanity_check_enabled_unit(self, unit_file: pathlib.Path) -> None:
        unit_name = unit_file.name
        if "@" in unit_name:
            unit_name = unit_name.replace("@", "@testinstance")
        self.sanity_check_enabled_unit_fragment(
            unit_name=unit_name, expected_unit_file=unit_file
        )
        self.sanity_check_enabled_unit_sources(
            unit_name=unit_name, expected_unit_file=unit_file
        )

    def sanity_check_enabled_unit_fragment(
        self, unit_name: SystemdUnitName, expected_unit_file: pathlib.Path
    ) -> None:
        service = SystemdService(unit_name=unit_name, systemd=self)
        actual_unit_file = service.query_fragment_path()
        if actual_unit_file != expected_unit_file:
            raise Exception(
                "Enabled unit's FragmentPath does not match unit file\n"
                "Expected: {repr(expected_unit_file)}\n"
                "Actual:   {repr(actual_unit_file)}"
            )

    def sanity_check_enabled_unit_sources(
        self, unit_name: SystemdUnitName, expected_unit_file: pathlib.Path
    ) -> None:
        actual_unit_sources = self._systemctl.check_output(["cat", "--", unit_name])

        expected_unit_sources = b""
        for file in [expected_unit_file]:
            expected_unit_sources += b"# " + bytes(file) + b"\n"
            expected_unit_sources += file.read_bytes()

        if actual_unit_sources != expected_unit_sources:
            raise Exception(
                "Enabled unit does not match unit file\n"
                "Expected: {repr(expected_unit_sources)}\n"
                "Actual:   {repr(actual_unit_sources)}"
            )

    def systemd_run(
        self,
        command: typing.Sequence[str],
        properties: typing.Mapping[str, str],
        extra_env: typing.Mapping[str, str],
        unit_name: typing.Optional[SystemdUnitName] = None,
    ) -> "SystemdService":
        systemd_run_command = ["systemd-run", "--user"]
        for name, value in properties.items():
            systemd_run_command.extend(("--property", f"{name}={value}"))
        for name, value in extra_env.items():
            systemd_run_command.extend(("--setenv", f"{name}={value}"))
        if unit_name is not None:
            systemd_run_command.extend(("--unit", unit_name))
        systemd_run_command.append("--")
        systemd_run_command.extend(command)

        output = subprocess.check_output(
            systemd_run_command, env=self.env, stderr=subprocess.STDOUT
        )
        match = re.match(
            r"^Running as unit: (?P<unit>.*)$",
            output.decode("utf-8"),
            flags=re.MULTILINE,
        )
        if match is None:
            raise Exception("Failed to parse unit from command output")
        return self.get_service(match.group("unit"))

    def get_active_unit_names(self) -> typing.List[SystemdUnitName]:
        def parse_line(line: str) -> SystemdUnitName:
            parts = re.split(r" +", line)
            return parts[0]

        stdout = self._systemctl.check_output(
            [
                "list-units",
                "--all",
                "--full",
                "--no-legend",
                "--no-pager",
                "--plain",
                "--state=active",
            ]
        )
        return [parse_line(line) for line in stdout.decode("utf-8").splitlines()]

    def get_unit_paths(self) -> typing.List[pathlib.Path]:
        stdout = subprocess.check_output(
            ["systemd-analyze", "--user", "unit-paths"], env=self.env
        )
        return [pathlib.Path(line) for line in stdout.decode("utf-8").splitlines()]

    def get_service(self, unit_name: SystemdUnitName) -> "SystemdService":
        return SystemdService(unit_name=unit_name, systemd=self)

    def exit(self) -> None:
        self._systemctl.check_call(["start", "exit.target"])

    @property
    def env(self) -> typing.Dict[str, str]:
        env = dict(os.environ)
        env.update(self.extra_env)
        return env

    @property
    def extra_env(self) -> typing.Dict[str, str]:
        return {
            "DBUS_SESSION_BUS_ADDRESS": "",
            "XDG_RUNTIME_DIR": str(self.xdg_runtime_dir),
        }

    @property
    def _systemctl(self) -> "_SystemctlCLI":
        return _SystemctlCLI(env=self.env)

    def __str__(self) -> str:
        return f"systemd ({self.xdg_runtime_dir})"

    def __repr__(self) -> str:
        return (
            f"SystemdUserServiceManager("
            f"xdg_runtime_dir={repr(self.xdg_runtime_dir)}, "
            f"process_id={self.process_id}"
            f")"
        )


class SystemdService:
    def __init__(
        self, unit_name: SystemdUnitName, systemd: SystemdUserServiceManager
    ) -> None:
        super().__init__()
        self.__systemd = systemd
        self.__unit_name = unit_name

    @property
    def unit_name(self) -> SystemdUnitName:
        return self.__unit_name

    def start(self) -> None:
        self.__systemctl.check_call(["start", "--", self.unit_name])

    def stop(self) -> None:
        self.__systemctl.check_call(["stop", "--", self.unit_name])

    def restart(self) -> None:
        self.__systemctl.check_call(["restart", "--", self.unit_name])

    def poll_until_inactive(self, timeout: float) -> None:
        def check_inactive() -> typing.Optional[bool]:
            return True if self.query_active_state() == "inactive" else None

        poll_until(check_inactive, timeout=timeout)

    def query_active_state(self) -> str:
        return self.__query_property("ActiveState").decode("utf-8")

    def query_sub_state(self) -> str:
        return self.__query_property("SubState").decode("utf-8")

    def query_main_process_id(self) -> typing.Optional[ProcessID]:
        return ProcessID(self.__query_property("MainPID"))

    def query_cgroup(self) -> LinuxCgroup:
        return LinuxCgroup(self.__query_property("ControlGroup"))

    def query_process_ids(self) -> typing.Sequence[ProcessID]:
        return self.query_cgroup().query_process_ids()

    def query_fragment_path(self) -> pathlib.Path:
        return pathlib.Path(os.fsdecode(self.__query_property("FragmentPath")))

    def __query_property(self, property: str) -> bytes:
        stdout = self.__systemctl.check_output(
            ["show", f"--property={property}", "--", self.unit_name]
        )
        prefix = property.encode("utf-8") + b"="
        if not stdout.startswith(prefix):
            raise Exception(f"Failed to parse output of systemctl show: {stdout}")
        return stdout[len(prefix) :].rstrip(b"\n")

    @property
    def __systemctl(self) -> "_SystemctlCLI":
        return self.__systemd._systemctl

    def __str__(self) -> str:
        return f"{self.unit_name} (XDG_RUNTIME_DIR={self.__systemd.xdg_runtime_dir})"

    def __repr__(self) -> str:
        return (
            f"SystemdService(unit_name={repr(self.unit_name)}, "
            f"systemd={repr(self.__systemd)})"
        )


class _SystemctlCLI:
    def __init__(self, env: typing.Dict[str, str]) -> None:
        super().__init__()
        self.__env = env

    def check_call(
        self, command_arguments: typing.Sequence[typing.Union[str, pathlib.Path]]
    ) -> None:
        """Run 'systemctl --user' with the given arguments.

        See also subprocess.check_call.
        """
        subprocess.check_call(self.__command(command_arguments), env=self.__env)

    def check_output(
        self, command_arguments: typing.Sequence[typing.Union[str, pathlib.Path]]
    ) -> bytes:
        """Run 'systemctl --user' and return the command's output.

        See also subprocess.check_output.
        """
        return subprocess.check_output(
            self.__command(command_arguments), env=self.__env
        )

    def run(
        self,
        command_arguments: typing.Sequence[typing.Union[str, pathlib.Path]],
        stdout: "subprocess._FILE" = None,
        stderr: "subprocess._FILE" = None,
    ) -> subprocess.CompletedProcess:
        """Run 'systemctl --user' and return the command's output and exit status.

        See also subprocess.run.
        """
        return subprocess.run(
            self.__command(command_arguments),
            env=self.__env,
            stdout=stdout,
            stderr=stderr,
        )

    def __command(
        self, command_arguments: typing.Sequence[typing.Union[str, pathlib.Path]]
    ) -> typing.Sequence[str]:
        command = ["systemctl", "--user"]
        command.extend(str(arg) for arg in command_arguments)
        return command


class SystemdUserServiceManagerMixin(metaclass=abc.ABCMeta):
    def make_temporary_systemd_user_service_manager(self) -> SystemdUserServiceManager:
        context_manager = temporary_systemd_user_service_manager()
        exit = context_manager.__exit__
        systemd = context_manager.__enter__()
        self.addCleanup(lambda: exit(None, None, None))
        return systemd

    def addCleanup(
        self,
        function: typing.Callable[..., typing.Any],
        *args: typing.Any,
        **kwargs: typing.Any,
    ) -> None:
        raise NotImplementedError()


@contextlib.contextmanager
def temporary_systemd_user_service_manager() -> typing.Iterator[
    SystemdUserServiceManager
]:
    """Create an isolated systemd instance for tests."""

    def should_create_managed() -> bool:
        forced_type_variable = "EDEN_TEST_FORCE_SYSTEMD_USER_SERVICE_MANAGER_TYPE"
        forced_type = os.getenv(forced_type_variable)
        if forced_type is not None and forced_type:
            if forced_type == "managed":
                return True
            if forced_type == "unmanaged":
                return False
            raise ValueError(
                f"Unsupported value for {forced_type_variable}: {forced_type!r}"
            )

        if not _is_system_booted_with_systemd():
            return False
        return True

    lifetime_duration = 30
    with create_tmp_dir() as xdg_runtime_dir:
        if should_create_managed():
            parent_systemd = SystemdUserServiceManager(
                xdg_runtime_dir=_get_current_xdg_runtime_dir(), process_id=None
            )
            with _transient_managed_systemd_user_service_manager(
                xdg_runtime_dir=xdg_runtime_dir,
                parent_systemd=parent_systemd,
                lifetime_duration=lifetime_duration,
            ) as child_systemd:
                yield child_systemd
        else:
            with _TransientUnmanagedSystemdUserServiceManager(
                xdg_runtime_dir=xdg_runtime_dir, lifetime_duration=lifetime_duration
            ) as systemd:
                yield systemd


def _is_system_booted_with_systemd() -> bool:
    """See the sd_booted(3) manual page."""
    return pathlib.Path("/run/systemd/system/").exists()


@contextlib.contextmanager
def _transient_managed_systemd_user_service_manager(
    xdg_runtime_dir: pathlib.Path,
    parent_systemd: SystemdUserServiceManager,
    lifetime_duration: int,
) -> typing.Iterator[SystemdUserServiceManager]:
    """Create an isolated systemd instance using 'systemd-run systemd'."""

    child_systemd_service = parent_systemd.systemd_run(
        command=["/usr/lib/systemd/systemd", "--user", "--unit=basic.target"],
        properties={
            "Description": f"Eden test systemd user service manager "
            f"({xdg_runtime_dir})",
            "CollectMode": "inactive-or-failed",
            "Restart": "no",
            "RuntimeMaxSec": str(lifetime_duration),
            "TimeoutStartSec": str(lifetime_duration),
            "Type": "notify",
        },
        extra_env={"XDG_RUNTIME_DIR": str(xdg_runtime_dir)},
    )
    child_systemd = SystemdUserServiceManager(
        xdg_runtime_dir=xdg_runtime_dir,
        process_id=child_systemd_service.query_main_process_id(),
    )
    try:
        yield child_systemd
    finally:
        try:
            child_systemd_service.stop()
        except Exception:
            logger.warning(
                f"Failed to stop systemd user service manager ({child_systemd})",
                exc_info=True,
            )
            # Ignore the exception.


class _TransientUnmanagedSystemdUserServiceManager:
    """Create an isolated systemd instance as child process.

    This class does not work if a user systemd instance is already running.
    """

    # pyre-ignore[13]: T38223649
    __cleanups: contextlib.ExitStack
    # pyre-ignore[13]: T38223649
    __lifetime_duration: int
    # pyre-ignore[13]: T38223649
    __xdg_runtime_dir: pathlib.Path

    def __init__(self, xdg_runtime_dir: pathlib.Path, lifetime_duration: int) -> None:
        super().__init__()
        self.__xdg_runtime_dir = xdg_runtime_dir
        self.__lifetime_duration = lifetime_duration
        self.__cleanups = contextlib.ExitStack()

    def start_systemd_process(self) -> subprocess.Popen:
        cgroup = self.create_cgroup()
        env = self.base_systemd_environment
        env["XDG_RUNTIME_DIR"] = str(self.__xdg_runtime_dir)
        # HACK(strager): Work around 'systemd --user' refusing to start if the
        # system is not managed by systemd.
        env["LD_PRELOAD"] = str(
            pathlib.Path(
                typing.cast(str, FindExe.FORCE_SD_BOOTED)  # T38947910
            ).resolve(strict=True)
        )
        process = subprocess.Popen(
            [
                "timeout",
                f"{self.__lifetime_duration}s",
                "/usr/lib/systemd/systemd",
                "--user",
                "--unit=basic.target",
                "--log-target=console",
            ],
            stdin=subprocess.DEVNULL,
            env=env,
            preexec_fn=lambda: cgroup.add_current_process(),
        )
        self.__cleanups.callback(lambda: self.stop_systemd_process(process))
        return process

    @property
    def base_systemd_environment(self) -> typing.Dict[str, str]:
        # See https://www.freedesktop.org/software/systemd/man/systemd.exec.html#Environment%20variables%20in%20spawned%20processes
        return {"PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"}

    def stop_systemd_process(self, systemd_process: subprocess.Popen) -> None:
        systemd_process.terminate()
        try:
            systemd_process.wait(timeout=15)
            return
        except subprocess.TimeoutExpired:
            pass

        logger.warning(
            "Failed to terminate systemd user service manager.  Attempting to kill."
        )
        systemd_process.kill()
        systemd_process.wait(timeout=3)

    def create_cgroup(self) -> LinuxCgroup:
        parent_cgroup = LinuxCgroup.from_current_process()
        path = tempfile.mkdtemp(
            prefix="edenfs_test.", dir=str(parent_cgroup.sys_fs_cgroup_path)
        )
        cgroup = LinuxCgroup.from_sys_fs_cgroup_path(pathlib.PosixPath(path))
        self.__cleanups.callback(lambda: cgroup.delete_recursive())
        return cgroup

    def wait_until_systemd_is_alive(
        self,
        systemd_process: subprocess.Popen,
        child_systemd: SystemdUserServiceManager,
    ) -> None:
        while True:
            systemd_did_exit = systemd_process.poll() is not None
            if systemd_did_exit:
                raise Exception("systemd failed to start")
            if child_systemd.is_alive():
                return

    def __enter__(self) -> SystemdUserServiceManager:
        systemd_process = self.start_systemd_process()
        child_systemd = SystemdUserServiceManager(
            xdg_runtime_dir=self.__xdg_runtime_dir, process_id=systemd_process.pid
        )
        self.wait_until_systemd_is_alive(systemd_process, child_systemd)
        return child_systemd

    def __exit__(
        self,
        exc_type: typing.Optional[typing.Type[BaseException]],
        exc_value: typing.Optional[BaseException],
        traceback: typing.Optional[types.TracebackType],
    ) -> typing.Optional[bool]:
        self.__cleanups.close()
        return None


def _get_current_xdg_runtime_dir() -> pathlib.Path:
    problems = []
    path = None

    if path is None:
        path_from_env = os.environ.get("XDG_RUNTIME_DIR")
        if path_from_env is None or path_from_env == "":
            problems.append("$XDG_RUNTIME_DIR is not set")
        else:
            path = pathlib.Path(path_from_env)

    if path is None:
        if os.getuid() == 0:
            path = pathlib.Path("/run")
        else:
            path = pathlib.Path("/run/user") / str(os.getuid())

    assert path is not None
    if not path.exists():
        problems.append(f"'{path}' does not exist")
        raise Exception(
            "Could not determine XDG_RUNTIME_DIR: " + ", and ".join(problems)
        )
    return path
