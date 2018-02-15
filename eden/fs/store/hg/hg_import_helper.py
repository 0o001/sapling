#!/usr/bin/env python2
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

from __future__ import absolute_import
from __future__ import division
from __future__ import print_function
from __future__ import unicode_literals

import argparse
import binascii
import collections
import logging
import os
import struct
import sys
import time

import mercurial.error
import mercurial.hg
import mercurial.node
import mercurial.scmutil
import mercurial.txnutil
import mercurial.util
import mercurial.ui

# remotefilelog is available just as "remotefilelog" in older mercurial
# releases, but "hgext.remotefilelog" moving forwards.
# TODO: Switch to unconditionally using hgext.remotefilelog once the
# new behavior is rolled out everywhere internally.
try:
    from hgext.remotefilelog import shallowutil, constants
except ImportError:
    from remotefilelog import shallowutil, constants

#
# Message chunk header format.
# (This is a format argument for struct.unpack())
#
# The header consists of 4 big-endian 32-bit unsigned integers:
#
# - Transaction ID
#   This is a numeric identifier used for associating a response with a given
#   request.  The response for a particular request will always contain the
#   same transaction ID as was sent in the request.  (Currently responses are
#   always sent in the same order that requests were received, so this is
#   primarily used just as a sanity check.)
#
# - Command ID
#   This is one of the CMD_* constants below.
#
# - Flags
#   This is a bit set of the FLAG_* constants defined below.
#
# - Data length
#   This lists the number of body bytes sent with this request/response.
#   The body is sent immediately after the header data.
#
HEADER_FORMAT = b'>IIII'
HEADER_SIZE = 16

# The length of a SHA-1 hash
SHA1_NUM_BYTES = 20

# The protocol version number.
#
# Increment this any time you add new commands or make changes to the data
# format sent between edenfs and the hg_import_helper.
#
# In general we do not need to worry about backwards/forwards compatibility of
# the protocol, since edenfs and the hg_import_helper.py script should always
# be updated together.  This protocol version ID allows us to sanity check that
# edenfs is actually talking to the correct hg_import_helper.py script,
# and to fail if it somehow is using an import helper script from the wrong
# release.
#
# This must be kept in sync with the PROTOCOL_VERSION field in the C++
# HgImporter code.
PROTOCOL_VERSION = 1

START_FLAGS_TREEMANIFEST_SUPPORTED = 0x01

#
# Message types.
#
# See the specific cmd_* functions below for documentation on the
# request/response formats.
#
CMD_STARTED = 0
CMD_RESPONSE = 1
CMD_MANIFEST = 2
CMD_CAT_FILE = 3
CMD_MANIFEST_NODE_FOR_COMMIT = 4
CMD_FETCH_TREE = 5

#
# Flag values.
#
# The flag values are intended to be bitwise-ORed with each other.
#

# FLAG_ERROR:
# - This flag is only valid in response chunks.  This indicates that an error
#   has occurred.  The chunk body contains the error message.  Any chunks
#   received prior to the error chunk should be ignored.
FLAG_ERROR = 0x01
# FLAG_MORE_CHUNKS:
# - If this flag is set, there are more chunks to come that are part of the
#   same request/response.  If this flag is not set, this is the final chunk in
#   this request/response.
FLAG_MORE_CHUNKS = 0x02


class Request(object):
    def __init__(self, txn_id, command, flags, body):
        self.txn_id = txn_id
        self.command = command
        self.flags = flags
        self.body = body


def cmd(command_id):
    '''
    A helper function for identifying command functions
    '''
    def decorator(func):
        func.__COMMAND_ID__ = command_id
        return func
    return decorator


class HgUI(mercurial.ui.ui):
    def __init__(self, src=None):
        super(HgUI, self).__init__(src=src)
        # Always print to stderr, never to stdout.
        # We normally use stdout as the pipe to communicate with the main
        # edenfs daemon, and if mercurial prints messages to stdout it can
        # interfere with this communication.
        # This also matches the logging behavior of the main edenfs process,
        # which always logs to stderr.
        self.fout = sys.stderr
        self.ferr = sys.stderr

    def interactive(self):
        return False


class HgServer(object):
    def __init__(self, repo_path, config_overrides, in_fd=None, out_fd=None):
        '''
        Create an HgServer.

        repo_path:
          The path to the mercurial repository
        config_overrides:
          A list of ConfigOption values, to be passed to ui.setconfig() when
          initializing the mercurial UI, after loading the normal config
          settings.  This is equivalent to specifying config options on the
          mercurial command line with `--config section.name=value`
        in_fd:
          A file descriptor to use for receiving requests.
          If in_fd is None, stdin will be used.
        out_fd:
          A file descriptor to use for sending responses.
          If in_fd is None, stdout will be used.
        '''
        self.repo_path = repo_path
        self.config_overrides = config_overrides
        if in_fd is None:
            self.in_file = sys.stdin
        else:
            self.in_file = os.fdopen(in_fd, 'rb')
        if out_fd is None:
            self.out_file = sys.stdout
        else:
            self.out_file = os.fdopen(out_fd, 'wb')

        # The repository will be set during initialized()
        self.repo = None
        self.ui = None

        # Populate our command dictionary
        self._commands = {}
        for member_name in dir(self):
            value = getattr(self, member_name)
            if not hasattr(value, '__COMMAND_ID__'):
                continue
            self._commands[value.__COMMAND_ID__] = value

    def initialize(self):
        self.ui = HgUI.load()
        for opt in self.config_overrides:
            self.ui.setconfig(opt.section, opt.name, opt.value,
                              source='--config')

        # Create a fresh copy of the UI object, and load the repository's
        # config into it.  Then load extensions specified by this config.
        hgrc = os.path.join(self.repo_path, b".hg", b"hgrc")
        local_ui = self.ui.copy()
        local_ui.readconfig(hgrc, self.repo_path)
        mercurial.extensions.loadall(local_ui)

        # Create the repository using the original clean UI object that has not
        # loaded the repo config yet.  This is required to ensure that
        # secondary repository objects end up with the correct configuration,
        # and do not have configuration settings from this repository.
        #
        # Secondary repo objects can be created mainly happens due to the share
        # extension.  In general the repository we are pointing at should
        # should not itself point to another shared repo, but it seems safest
        # to exactly mimic mercurial's own start-up behavior here.
        repo = mercurial.hg.repository(self.ui, self.repo_path)
        self.repo = repo.unfiltered()

        try:
            self.treemanifest = mercurial.extensions.find('treemanifest')
        except KeyError:
            # The treemanifest extension is not present
            self.treemanifest = None

    def serve(self):
        try:
            self.initialize()
        except Exception as ex:
            # If an error occurs during initialization (say, if the repository
            # path is invalid), send an error response.
            self.send_exception(request=None, exc=ex)
            return 1

        # Send a CMD_STARTED response to indicate we have started,
        # and include some information about the repository configuration.
        options_chunk = self._gen_options()
        self._send_chunk(txn_id=0, command=CMD_STARTED,
                         flags=0, data=options_chunk)

        while self.process_request():
            pass

        logging.debug('hg_import_helper shutting down normally')
        return 0

    def _gen_options(self):
        use_treemanifest = ((self.treemanifest is not None) and
                            bool(getattr(self.repo, 'name', None)))

        flags = 0
        treemanifest_paths = []
        if use_treemanifest:
            flags |= START_FLAGS_TREEMANIFEST_SUPPORTED
            treemanifest_paths = [
                shallowutil.getlocalpackpath(self.repo.svfs.vfs.base,
                                             constants.TREEPACK_CATEGORY),
                shallowutil.getcachepackpath(self.repo,
                                             constants.TREEPACK_CATEGORY),
            ]

        # Options format:
        # - Protocol version number
        # - Is treemanifest supported?
        # - Number of treemanifest paths
        #   - treemanifest paths, encoded as (length, string_data)
        parts = []
        parts.append(struct.pack(b'>III', PROTOCOL_VERSION, flags,
                                 len(treemanifest_paths)))
        for path in treemanifest_paths:
            parts.append(struct.pack(b'>I', len(path)))
            parts.append(path)

        return ''.join(parts)

    def debug(self, msg, *args, **kwargs):
        logging.debug(msg, *args, **kwargs)

    def process_request(self):
        # Read the request header
        header_data = self.in_file.read(HEADER_SIZE)
        if not header_data:
            # EOF.  All done serving
            return False

        if len(header_data) < HEADER_SIZE:
            raise Exception('received EOF after partial request header')

        header_fields = struct.unpack(HEADER_FORMAT, header_data)
        txn_id, command, flags, data_len = header_fields

        # Read the request body
        body = self.in_file.read(data_len)
        if len(body) < data_len:
            raise Exception('received EOF after partial request')
        req = Request(txn_id, command, flags, body)

        cmd_function = self._commands.get(command)
        if cmd_function is None:
            logging.warning('unknown command %r', command)
            self.send_error(req, 'CommandError',
                            'unknown command %r' % (command,))
            return True

        try:
            cmd_function(req)
        except Exception as ex:
            logging.exception('error processing command %r', command)
            self.send_exception(req, ex)

        # Return True to indicate that we should continue serving
        return True

    @cmd(CMD_MANIFEST)
    def cmd_manifest(self, request):
        '''
        Handler for CMD_MANIFEST requests.

        This request asks for the full mercurial manifest contents for a given
        revision.  The response body will be split across one or more chunks.
        (FLAG_MORE_CHUNKS will be set on all but the last chunk.)

        Request body format:
        - Revision name (string)
          This is the mercurial revision ID.  This can be any string that will
          be understood by mercurial to identify a single revision.  (For
          instance, this might be ".", ".^", a 40-character hexadecmial hash,
          or a unique hash prefix, etc.)

        Response body format:
          The response body is a list of manifest entries.  Each manifest entry
          consists of:
          - <rev_hash><tab><flag><tab><path><nul>

          Entry fields:
          - <rev_hash>: The file revision hash, as a 20-byte binary value.
          - <tab>: A literal tab character ('\t')
          - <flag>: The mercurial flag character.  If the mercurial flag is
                    empty this will be omitted.  Valid mercurial flags are:
                    'x': an executable file
                    'l': an symlink
                    '':  a regular file
          - <path>: The full file path, relative to the root of the repository
          - <nul>: a nul byte ('\0')
        '''
        rev_name = request.body
        self.debug('sending manifest for revision %r', rev_name)
        self.dump_manifest(rev_name, request)

    @cmd(CMD_CAT_FILE)
    def cmd_cat_file(self, request):
        '''
        Handler for CMD_CAT_FILE requests.

        This requests the contents for a given file.

        Request body format:
        - <rev_hash><path>
          Fields:
          - <rev_hash>: The file revision hash, as a 20-byte binary value.
          - <path>: The file path, relative to the root of the repository.

        Response body format:
        - <file_contents>
          The body consists solely of the raw file contents.
        '''
        if len(request.body) < SHA1_NUM_BYTES + 1:
            raise Exception('cat_file request data too short')

        rev_hash = request.body[:SHA1_NUM_BYTES]
        path = request.body[SHA1_NUM_BYTES:]
        self.debug('(pid:%s) getting contents of file %r revision %s',
                   os.getpid(),
                   path,
                   binascii.hexlify(rev_hash))

        contents = self.get_file(path, rev_hash)
        self.send_chunk(request, contents)

    @cmd(CMD_MANIFEST_NODE_FOR_COMMIT)
    def cmd_manifest_node_for_commit(self, request):
        '''
        Handler for CMD_MANIFEST_NODE_FOR_COMMIT requests.

        Given a commit hash, resolve the manifest node.

        Request body format:
        - Revision name (string)
          This is the mercurial revision ID.  This can be any string that will
          be understood by mercurial to identify a single revision.  (For
          instance, this might be ".", ".^", a 40-character hexadecmial hash,
          or a unique hash prefix, etc.)

        Response body format:
          The response body is the manifest node, a 20-byte binary value.
        '''
        rev_name = request.body
        self.debug('resolving manifest node for revision %r', rev_name)
        try:
            node = self.get_manifest_node(rev_name)
        except mercurial.error.RepoError as ex:
            # Handle lookup errors explicitly, just so we avoid printing
            # a backtrace in the log if we let this bubble all the way up
            # to the unexpected exception handling code in process_request()
            self.send_exception(request, ex)
            return

        self.send_chunk(request, node)

    @cmd(CMD_FETCH_TREE)
    def cmd_fetch_tree(self, request):
        if len(request.body) < SHA1_NUM_BYTES:
            raise Exception('fetch_tree request data too short: len=%d' %
                            len(request.body))

        manifest_node = request.body[:SHA1_NUM_BYTES]
        path = request.body[SHA1_NUM_BYTES:]
        self.debug('fetching tree for path %r manifest node %s',
                   path, binascii.hexlify(manifest_node))

        self.fetch_tree(path, manifest_node)
        self.send_chunk(request, b'')

    def fetch_tree(self, path, manifest_node):
        if self.treemanifest is None:
            raise Exception('treemanifest not enabled in this repository')

        mfnodes = set([manifest_node])
        base_mfnodes = set()

        # The directories parameter isn't actually supported and
        # must always be an empty list.
        directories = []

        # It would be nice to initially only fetch the one tree that we need
        # immediately, and fetch the rest of the subtree later, in the
        # background.  Unfortunately the wire protocol API does not support a
        # mechanism to do this yet.  In the future it's probably worth adding a
        # "depth" parameter requesting data only down to a specific depth.

        # Newer mercurial releases have self.repo.prefetchtrees()
        # Older mercurial releases have self.treemanifest._prefetchtrees()
        if mercurial.util.safehasattr(self.repo, 'prefetchtrees'):
            # TODO: repo.prefetchtrees() does not accept a path
            self.repo.prefetchtrees(mfnodes)
        else:
            self.treemanifest._prefetchtrees(self.repo, path, mfnodes,
                                             base_mfnodes, directories)

    def send_chunk(self, request, data, is_last=True):
        flags = 0
        if not is_last:
            flags |= FLAG_MORE_CHUNKS
        self._send_chunk(request.txn_id, command=CMD_RESPONSE,
                         flags=flags, data=data)

    def send_exception(self, request, exc):
        self.send_error(request, type(exc).__name__, str(exc))

    def send_error(self, request, error_type, message):
        txn_id = 0
        if request is not None:
            txn_id = request.txn_id

        data = b''.join([
            struct.pack(b'>I', len(error_type)),
            error_type,
            struct.pack(b'>I', len(message)),
            message,
        ])
        self._send_chunk(txn_id, command=CMD_RESPONSE,
                         flags=FLAG_ERROR, data=data)

    def _send_chunk(self, txn_id, command, flags, data):
        header = struct.pack(HEADER_FORMAT, txn_id, command, flags,
                             len(data))
        self.out_file.write(header)
        self.out_file.write(data)
        self.out_file.flush()

    def dump_manifest(self, rev, request):
        '''
        Send the manifest data.
        '''
        start = time.time()
        try:
            ctx = mercurial.scmutil.revsingle(self.repo, rev)
            mf = ctx.manifest()
        except Exception:
            # The mercurial call may fail with a "no node" error if this
            # revision in question has added to the repository after we
            # originally opened it.  Invalidate the repository and try again,
            # in case our cached repo data is just stale.
            self.repo.invalidate(clearfilecache=True)
            ctx = mercurial.scmutil.revsingle(self.repo, rev)
            mf = ctx.manifest()

        # How many paths to send in each chunk
        # Empirically, 100 seems like a decent number.
        # Too small and we pay a cost for doing too many small writes.
        # Too big and the C++ code is idle while it waits for us to build a
        # chunk, and then we fill up the pipe writing the data out, and have
        # to wait for it to be processed before we can start building the next
        # chunk.
        MANIFEST_PATHS_PER_CHUNK = 100

        chunked_paths = []
        num_paths = 0
        for path, hashval, flags in mf.iterentries():
            # Construct the chunk data using join(), since that is relatively
            # fast compared to other ways of constructing python strings.
            entry = b'\t'.join((hashval, flags, path + b'\0'))
            if len(chunked_paths) >= MANIFEST_PATHS_PER_CHUNK:
                num_paths += len(chunked_paths)
                self.send_chunk(request, b''.join(chunked_paths),
                                is_last=False)
                chunked_paths = [entry]
            else:
                chunked_paths.append(entry)

        num_paths += len(chunked_paths)
        self.send_chunk(request, b''.join(chunked_paths), is_last=True)
        self.debug('sent manifest with %d paths in %s seconds',
                   num_paths, time.time() - start)

    def get_manifest_node(self, rev):
        try:
            ctx = mercurial.scmutil.revsingle(self.repo, rev)
            return ctx.manifestnode()
        except Exception:
            # The mercurial call may fail with a "no node" error if this
            # revision in question has added to the repository after we
            # originally opened it.  Invalidate the repository and try again,
            # in case our cached repo data is just stale.
            #
            # clearfilecache=True is necessary so that mercurial will open
            # 00changelog.i.a if it exists now instead of just using
            # 00changelog.i  The .a file contains pending commit data if a
            # transaction is in progress.
            self.repo.invalidate(clearfilecache=True)
            ctx = mercurial.scmutil.revsingle(self.repo, rev)
            return ctx.manifestnode()

    def get_file(self, path, rev_hash):
        try:
            fctx = self.repo.filectx(path, fileid=rev_hash)
        except Exception:
            self.repo.invalidate()
            fctx = self.repo.filectx(path, fileid=rev_hash)
        return fctx.data()

    def prefetch(self, rev):
        if not hasattr(self.repo, 'prefetch'):
            # This repo isn't using remotefilelog, so nothing to do.
            return

        try:
            rev_range = mercurial.scmutil.revrange(self.repo, rev)
        except Exception:
            self.repo.invalidate()
            rev_range = mercurial.scmutil.revrange(self.repo, rev)

        self.debug('prefetching')
        self.repo.prefetch(rev_range)
        self.debug('done prefetching')


def always_allow_pending(root):
    return True


ConfigOption = collections.namedtuple('ConfigOption',
                                      ['section', 'name', 'value'])


def parse_config_options(argparser, options):
    '''
    Parse config options specified using --config arguments.

    The options parameter should be the list of --config option values.
    Each option value should be of the form "section.name=value"

    This function returns a list of ConfigOption objects.
    '''
    results = []
    for option in options:
        try:
            name, value = [element.strip() for element in option.split('=', 1)]
            section, name = name.split('.', 1)
            results.append(ConfigOption(section, name, value))
        except (IndexError, ValueError):
            argparser.error('bad --config argument %r: must be of the form '
                            'SECTION.NAME=VALUE' % (option,))
    return results


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('repo', help='The repository path')
    parser.add_argument('--config',
                        metavar='SECTION.NAME=VALUE', action='append',
                        default=[],
                        help='Specify mercurial configuration options')
    parser.add_argument('--in-fd',
                        metavar='FILENO', type=int,
                        help='Use the specified file descriptor to receive '
                        'commands, rather than reading on stdin')
    parser.add_argument('--out-fd',
                        metavar='FILENO', type=int,
                        help='Use the specified file descriptor to send '
                        'command output, rather than writing to stdout')

    # Arguments for testing and debugging.
    # These cause the helper to perform a single operation and exit,
    # rather than running as a server.
    parser.add_argument('--manifest',
                        metavar='REVISION',
                        help='Dump the binary manifest data for the specified '
                        'revision.')
    parser.add_argument('--get-manifest-node',
                        metavar='REVISION',
                        help='Print the manifest node ID for the specified '
                        'revision.')
    parser.add_argument('--cat-file',
                        metavar='PATH:REV',
                        help='Dump the file contents for the specified file '
                        'at the given file revision')
    parser.add_argument('--fetch-tree',
                        metavar='PATH:REV',
                        help='Fetch treemanifest data for the specified path '
                        'at the given manifest node')

    args = parser.parse_args()
    config_overrides = parse_config_options(parser, args.config)

    logging.basicConfig(stream=sys.stderr, level=logging.INFO,
                        format='%(asctime)s %(message)s')

    # We always want to be able to access commits being created by pending
    # transactions.
    #
    # Monkey-patch mercural.txnutil and replace its _mayhavepending() function
    # with one that always returns True.  We could set the HG_PENDING
    # environment variable to try and get it to return True without
    # monkey-patching, but this seems a bit more fragile--it requires an exact
    # string match on the repository path, so we would have to make sure to
    # normalize the repository path the same way mercurial does, and make sure
    # we use the correct repository (in case of a shared repository).
    mercurial.txnutil.mayhavepending = always_allow_pending

    server = HgServer(args.repo, config_overrides,
                      in_fd=args.in_fd, out_fd=args.out_fd)

    if args.get_manifest_node:
        server.initialize()
        node = server.get_manifest_node(args.get_manifest_node)
        print(binascii.hexlify(node))
        return 0

    if args.manifest is not None:
        server.initialize()
        request = Request(0, CMD_MANIFEST, flags=0, body=args.manifest)
        server.dump_manifest(args.manifest, request)
        return 0

    if args.cat_file is not None:
        server.initialize()
        path, file_rev_str = args.cat_file.rsplit(':', -1)
        path = path.encode(sys.getfilesystemencoding())
        file_rev = binascii.unhexlify(file_rev_str)
        data = server.get_file(path, file_rev)
        sys.stdout.write(data)
        return 0

    if args.fetch_tree is not None:
        server.initialize()
        parts = args.fetch_tree.rsplit(':', -1)
        if len(parts) == 1:
            path = parts[0]
            if path == '':
                manifest_node = server.get_manifest_node('.')
            else:
                # TODO: It would be nice to automatically look up the current
                # manifest node ID for this path and use that here, assuming
                # we have sufficient data locally for this
                raise Exception('a manifest node ID is required when '
                                'using a path')
        else:
            path, manifest_node_str = parts
            manifest_node = binascii.unhexlify(manifest_node_str)
            if len(manifest_node) != 20:
                raise Exception('manifest node should be a 40-byte hex string')

        server.fetch_tree(path, manifest_node)
        return 0

    try:
        return server.serve()
    except KeyboardInterrupt:
        logging.debug('hg_import_helper received interrupt; shutting down')


if __name__ == '__main__':
    rc = main()
    sys.exit(rc)
