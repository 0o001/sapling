#!/usr/bin/env python3
# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

import ctypes
import datetime
import sys
import types
from ctypes.wintypes import BOOL as _BOOL, DWORD as _DWORD, HANDLE as _HANDLE
from pathlib import Path
from typing import Iterable, NoReturn, Optional, Type

from . import proc_utils


if sys.platform == "win32":
    _win32 = ctypes.windll.kernel32
    _win32.OpenProcess.argtypes = [_DWORD, _BOOL, _DWORD]
    _win32.OpenProcess.restype = _HANDLE

    def raise_win_error() -> NoReturn:
        raise ctypes.WinError()


else:
    # This entire file is only ever imported in Windows.  However on our continuous
    # integration environments Pyre currently does all of its type checking assuming
    # Linux.  Define a fake kernel32 module just to allow it to still perform some type
    # checking for us.
    class _win32:
        @staticmethod
        def OpenProcess(desired_access: int, inherit_handle: bool, pid: int) -> _HANDLE:
            ...

        @staticmethod
        def CloseHandle(handle: _HANDLE) -> None:
            ...

        @staticmethod
        def TerminateProcess(handle: _HANDLE, exit_code: int) -> bool:
            ...

    def raise_win_error() -> NoReturn:
        ...


_PROCESS_TERMINATE = 0x0001
_PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
_ERROR_ACCESS_DENIED = 5
_CREATE_NO_WINDOW = 0x08000000


class Handle:
    handle: _HANDLE

    def __init__(self, handle: _HANDLE) -> None:
        self.handle = handle

    def __enter__(self) -> "Handle":
        return self

    def __exit__(
        self,
        exc_type: Optional[Type[BaseException]],
        exc_value: Optional[BaseException],
        tb: Optional[types.TracebackType],
    ) -> None:
        self.close()

    def close(self) -> None:
        if self.handle:
            _win32.CloseHandle(self.handle)
            self.handle = _HANDLE()


def open_process(pid: int, access: int = _PROCESS_QUERY_LIMITED_INFORMATION) -> Handle:
    handle_value = _win32.OpenProcess(access, False, pid)
    if handle_value is None:
        raise_win_error()
    return Handle(handle_value)


class WinProcUtils(proc_utils.ProcUtils):
    def get_edenfs_processes(self) -> Iterable[proc_utils.EdenFSProcess]:
        # TODO: Finding all EdenFS processes is not yet implemented on Windows
        # This function is primarily used by `eden doctor` and other tools looking for
        # stale EdenFS instances on the system.  Returning an empty list for now will
        # allow those tools to run but just not find any stale processes.
        return []

    def get_process_start_time(self, pid: int) -> float:
        raise NotImplementedError(
            "Windows does not currently implement get_process_start_time()"
        )

    def is_system_idle(
        self, tty_idle_timeout: datetime.timedelta, root_path: Path
    ) -> bool:
        raise NotImplementedError(
            "Windows does not currently implement is_system_idle()"
        )

    def kill_process(self, pid: int) -> None:
        with open_process(pid, _PROCESS_TERMINATE) as p:
            exit_code = 1
            if not _win32.TerminateProcess(p.handle, exit_code):
                raise_win_error()

    def is_process_alive(self, pid: int) -> bool:
        try:
            handle = open_process(pid)
        except PermissionError:
            # The process exists, but we don't have permission to query it.
            return True
        except OSError:
            return False

        handle.close()
        return True

    def is_edenfs_process(self, pid: int) -> bool:
        # For now we just check that the process exists.
        # In the future it might be nice to also check if the command line or executable
        # looks like EdenFS, but this is sufficient for now.
        return self.is_process_alive(pid)
