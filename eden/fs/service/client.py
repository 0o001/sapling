# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from __future__ import absolute_import, division, print_function, unicode_literals

import os
from typing import Any, Iterator, Optional, cast  # noqa: F401

from facebook.eden import EdenService
from thrift.protocol.THeaderProtocol import THeaderProtocol
from thrift.Thrift import TApplicationException
from thrift.transport.THeaderTransport import THeaderTransport
from thrift.transport.TSocket import TSocket
from thrift.transport.TTransport import TTransportException


SOCKET_PATH = "socket"


class EdenNotRunningError(Exception):
    def __init__(self, eden_dir):
        msg = "edenfs daemon does not appear to be running: tried %s" % eden_dir
        super(EdenNotRunningError, self).__init__(msg)
        self.eden_dir = eden_dir


# Monkey-patch EdenService.EdenError's __str__() behavior to just return the
# error message.  By default it returns the same data as __repr__(), which is
# ugly to show to users.
def _eden_thrift_error_str(ex):
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
        if socket_path is not None:
            self._socket_path = socket_path
        elif eden_dir is not None:
            self._socket_path = os.path.join(eden_dir, SOCKET_PATH)
        else:
            raise TypeError("one of eden_dir or socket_path is required")
        self._socket = TSocket(unix_socket=self._socket_path)
        # We used to set a timeout here, but picking the right duration is hard,
        # and safely retrying an arbitrary thrift call may not be safe.  So we
        # just leave the client with no timeout.
        # self.set_timeout(60)
        self.set_timeout(None)
        self._transport = THeaderTransport(self._socket)
        self._protocol = THeaderProtocol(self._transport)
        super(EdenClient, self).__init__(self._protocol)

    def __enter__(self):
        self.open()
        return self

    def __exit__(self, exc_type, exc_value, exc_traceback):
        self.close()

    def open(self):
        try:
            self._transport.open()
        except TTransportException as ex:
            self.close()
            if ex.type == TTransportException.NOT_OPEN:
                raise EdenNotRunningError(self._socket_path)
            raise

    def close(self):
        if self._transport is not None:
            self._transport.close()
            self._transport = None

    def shutdown(self):
        # type: () -> None
        self.initiateShutdown(
            "EdenClient.shutdown() invoked with no reason by pid=%s uid=%s"
            % (os.getpid(), os.getuid())
        )

    def initiateShutdown(self, reason):
        # type: (str) -> None
        """Helper for stopping the server.
        To swing through the transition from calling the base shutdown() method
        with context to the initiateShutdown() method with a reason, we want to
        try the latter method first, falling back to the old way to handle the
        case where we deploy a newer client while an older server is still
        running on the local system."""
        try:
            super().initiateShutdown(reason)
        except TApplicationException as ex:
            if ex.type == TApplicationException.UNKNOWN_METHOD:
                # Running an older server build, fall back to the old shutdown
                # method with no context
                super().shutdown()
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
