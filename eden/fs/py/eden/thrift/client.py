# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from __future__ import absolute_import, division, print_function, unicode_literals

import os
import sys
from typing import Any, Optional, cast  # noqa: F401

from facebook.eden import EdenService
from facebook.eden.ttypes import DaemonInfo
from thrift.protocol.THeaderProtocol import THeaderProtocol
from thrift.Thrift import TApplicationException
from thrift.transport.THeaderTransport import THeaderTransport
from thrift.transport.TTransport import TTransportException


if sys.platform == "win32":
    from eden.thrift.windows_thrift import WinTSocket  # @manual
else:
    from thrift.transport.TSocket import TSocket


SOCKET_PATH = "socket"


class EdenNotRunningError(Exception):
    def __init__(self, eden_dir):
        # type: (str) -> None
        msg = "edenfs daemon does not appear to be running: tried %s" % eden_dir
        super(EdenNotRunningError, self).__init__(msg)
        self.eden_dir = eden_dir


# Monkey-patch EdenService.EdenError's __str__() behavior to just return the
# error message.  By default it returns the same data as __repr__(), which is
# ugly to show to users.
def _eden_thrift_error_str(ex):
    # type: (EdenService.EdenError) -> str
    return ex.message


# TODO: https://github.com/python/mypy/issues/2427
cast(Any, EdenService.EdenError).__str__ = _eden_thrift_error_str


class EdenClient(EdenService.Client):
    """
    EdenClient is a subclass of EdenService.Client that provides
    a few additional conveniences:

    - Smarter constructor
    - Implement the context manager __enter__ and __exit__ methods, so it can
      be used in with statements.
    """

    def __init__(self, eden_dir=None, socket_path=None):
        # type: (Optional[str], Optional[str]) -> None
        if socket_path is not None:
            self._socket_path = socket_path
        elif eden_dir is not None:
            self._socket_path = os.path.join(eden_dir, SOCKET_PATH)
        else:
            raise TypeError("one of eden_dir or socket_path is required")
        if sys.platform == "win32":
            self._socket = WinTSocket(unix_socket=self._socket_path)
        else:
            self._socket = TSocket(unix_socket=self._socket_path)

        # We used to set a timeout here, but picking the right duration is hard,
        # and safely retrying an arbitrary thrift call may not be safe.  So we
        # just leave the client with no timeout.
        # self.set_timeout(60)
        self.set_timeout(None)
        transport = THeaderTransport(self._socket)
        self._transport = transport  # type: Optional[THeaderTransport]
        self._protocol = THeaderProtocol(transport)
        super(EdenClient, self).__init__(self._protocol)

    def __enter__(self):
        # type: () -> EdenClient
        self.open()
        return self

    def __exit__(self, exc_type, exc_value, exc_traceback):
        # type: (Any, Any, Any) -> Optional[bool]
        self.close()
        return False

    def open(self):
        # type: () -> None
        try:
            assert self._transport is not None
            # pyre-fixme[16]: `Optional` has no attribute `open`.
            self._transport.open()
        except TTransportException as ex:
            self.close()
            if ex.type == TTransportException.NOT_OPEN:
                raise EdenNotRunningError(self._socket_path)
            raise

    def close(self):
        # type: () -> None
        if self._transport is not None:
            # pyre-fixme[16]: `Optional` has no attribute `close`.
            self._transport.close()
            self._transport = None

    def getDaemonInfo(self):
        # type: () -> DaemonInfo
        try:
            info = super(EdenClient, self).getDaemonInfo()
        except TApplicationException as ex:
            if ex.type != TApplicationException.UNKNOWN_METHOD:
                raise
            # Older versions of EdenFS did not have a getDaemonInfo() method
            pid = super(EdenClient, self).getPid()
            info = DaemonInfo(pid=pid, status=None)

        # Older versions of EdenFS did not return status information in the
        # getDaemonInfo() response.
        if info.status is None:
            info.status = super(EdenClient, self).getStatus()
        return info

    def getPid(self):
        # type: () -> int
        try:
            return self.getDaemonInfo().pid
        except TApplicationException as ex:
            if ex.type == TApplicationException.UNKNOWN_METHOD:
                # Running on an older server build, fall back to the
                # old getPid() method.
                return super(EdenClient, self).getPid()
            else:
                raise

    def set_timeout(self, timeout):
        # type: (Optional[float]) -> None
        if timeout is None:
            timeout_ms = None
        else:
            timeout_ms = timeout * 1000
        self.set_timeout_ms(timeout_ms)

    def set_timeout_ms(self, timeout_ms):
        # type: (Optional[float]) -> None
        self._socket.setTimeout(timeout_ms)


def create_thrift_client(eden_dir=None, socket_path=None):
    # type: (Optional[str], Optional[str]) -> EdenClient
    """Construct a thrift client to speak to the running eden server
    instance associated with the specified mount point.

    @return Returns a context manager for EdenService.Client.
    """
    return EdenClient(eden_dir=eden_dir, socket_path=socket_path)
