#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import argparse
from eden.thrift import EdenNotRunningError
import errno
import json
import os
import signal
import subprocess
import sys

from . import config as config_mod
from . import debug as debug_mod
from . import rage as rage_mod
from . import stats as stats_mod
from . import util
from .cmd_util import create_config
from facebook.eden import EdenService


def infer_client_from_cwd(config, clientname):
    if clientname:
        return clientname

    all_clients = config.get_all_client_config_info()
    path = normalize_path_arg(os.getcwd())

    # Keep going while we're not in the root, as dirname(/) is /
    # and we can keep iterating forever.
    while len(path) > 1:
        for _, info in all_clients.items():
            if info['mount'] == path:
                return info['mount']
        path = os.path.dirname(path)

    print_stderr(
        'cwd is not an eden mount point, and no client name was specified.')
    sys.exit(2)


def do_help(args, parser, subparsers):
    help_args = getattr(args, 'args', [])
    num_help_args = len(help_args)
    if num_help_args == 1:
        name = args.args[0]
        subparser = subparsers.choices.get(name, None)
        if subparser:
            subparser.parse_args(['--help'])
        else:
            print_stderr('No manual entry for %s' % name)
            sys.exit(2)
    elif num_help_args == 0:
        parser.parse_args(['--help'])
    else:
        print_stderr('Too many args passed to help: %s' % help_args)
        sys.exit(2)


def do_info(args):
    config = create_config(args)
    info = config.get_client_info(infer_client_from_cwd(config, args.client))
    json.dump(info, sys.stdout, indent=2)
    sys.stdout.write('\n')


def do_health(args):
    config = create_config(args)
    health_info = config.check_health()
    if health_info.is_healthy():
        print('eden running normally (pid {})'.format(health_info.pid))
        return 0

    print('edenfs not healthy: {}'.format(health_info.detail))
    return 1


def do_repository(args):
    config = create_config(args)
    if (args.name and args.path):
        repo_source, repo_type = util.get_repo_source_and_type(args.path)
        if repo_type is None:
            print_stderr(
                '%s does not look like a git or hg repository' % args.path)
            return 1
        try:
            config.add_repository(args.name,
                                  repo_type=repo_type,
                                  source=repo_source,
                                  with_buck=args.with_buck)
        except config_mod.UsageError as ex:
            print_stderr('error: {}', ex)
            return 1
    elif (args.name or args.path):
        print_stderr('repository command called with incorrect arguments')
        return 1
    else:
        repo_list = config.get_repository_list()
        for repo in sorted(repo_list):
            print(repo)


def do_list(args):
    config = create_config(args)
    for path in config.get_mount_paths():
        print(path)


def do_clone(args):
    args.path = normalize_path_arg(args.path)
    config = create_config(args)
    snapshot_id = args.snapshot
    if not snapshot_id:
        try:
            source = config.get_repo_data(args.repo)
        except Exception as ex:
            print_stderr('error: {}', ex)
            return 1

        if source['type'] == 'git':
            snapshot_id = util.get_git_commit(source['path'])
        elif source['type'] == 'hg':
            snapshot_id = util.get_hg_commit(source['path'])
        else:
            print_stderr(
                '%s does not look like a git or hg repository' % args.path)
            return 1
    try:
        return config.clone(args.repo, args.path, snapshot_id)
    except Exception as ex:
        print_stderr('error: {}', ex)
        return 1


def do_config(args):
    config = create_config(args)

    if args.get:
        try:
            print(config.get_config_value(args.get))
        except (KeyError, ValueError):
            # mirrors `git config --get invalid`; just exit with code 1
            return 1
    else:
        config.print_full_config()
    return 0

def do_mount(args):
    config = create_config(args)
    try:
        return config.mount(args.path)
    except EdenNotRunningError as ex:
        print_stderr('error: {}', ex)
        return 1


def do_unmount(args):
    args.path = normalize_path_arg(args.path)
    config = create_config(args)
    try:
        return config.unmount(args.path, delete_config=not args.no_forget)
    except EdenService.EdenError as ex:
        print_stderr('error: {}', ex)
        return 1


def do_checkout(args):
    config = create_config(args)
    try:
        config.checkout(infer_client_from_cwd(config, args.client),
                        args.snapshot)
    except Exception as ex:
        print_stderr('checkout of %s failed for client %s: %s' % (
                     args.snapshot,
                     args.client,
                     str(ex)))
        sys.exit(1)


def do_daemon(args):
    config = create_config(args)
    daemon_binary = args.daemon_binary or _find_default_daemon_binary()

    # If this is the first time running the daemon, the ~/.eden directory
    # structure needs to be set up.
    # TODO(mbolin): Check whether the user is running as sudo/root. In general,
    # we want to avoid creating ~/.eden as root.
    _ensure_dot_eden_folder_exists(config)

    # If the user put an "--" argument before the edenfs args, argparse passes
    # that through to us.  Strip it out.
    edenfs_args = args.edenfs_args
    if edenfs_args and edenfs_args[0] == '--':
        edenfs_args = edenfs_args[1:]

    try:
        health_info = config.spawn(daemon_binary, edenfs_args,
                                   gdb=args.gdb, gdb_args=args.gdb_arg,
                                   strace_file=args.strace,
                                   foreground=args.foreground)
    except config_mod.EdenStartError as ex:
        print_stderr('error: {}', ex)
        return 1
    print('Started edenfs (pid {}). Logs available at {}'.format(
        health_info.pid, config.get_log_path()))
    return 0


def do_rage(args):
    rage_processor = None
    config = create_config(args)
    try:
        rage_processor = config.get_config_value('rage.reporter')
    except KeyError:
        pass

    if rage_processor:
        proc = subprocess.Popen(['sh', '-c', rage_processor], stdin=subprocess.PIPE)
        sink = proc.stdin
    else:
        proc = None
        sink = sys.stdout.buffer

    rage_mod.print_diagnostic_info(config, args, sink)
    if proc:
        sink.close()
        proc.wait()
    return 0


def do_stats(args):
    stats_mod.do_stat_general(args)
    return 0


def _find_default_daemon_binary():
    # By default, we look for the daemon executable alongside this file.
    script_dir = os.path.dirname(os.path.abspath(sys.argv[0]))
    candidate = os.path.join(script_dir, 'edenfs')
    permissions = os.R_OK | os.X_OK
    if os.access(candidate, permissions):
        return candidate

    # This is where the binary will be found relative to this file when it is
    # run out of buck-out in debug mode.
    candidate = os.path.normpath(os.path.join(script_dir, '../fs/service/edenfs'))
    if os.access(candidate, permissions):
        return candidate
    else:
        return None


def _ensure_dot_eden_folder_exists(config):
    '''Creates the ~/.eden folder as specified by --config-dir/$EDEN_CONFIG_DIR.
    If the ~/.eden folder already exists, it will be left alone.

    Returns the path to the RocksDB.
    '''
    db = config.get_or_create_path_to_rocks_db()
    return db


SHUTDOWN_EXIT_CODE_NORMAL = 0
SHUTDOWN_EXIT_CODE_REQUESTED_SHUTDOWN = 0
SHUTDOWN_EXIT_CODE_NOT_RUNNING_ERROR = 2
SHUTDOWN_EXIT_CODE_TERMINATED_VIA_SIGKILL = 3
SHUTDOWN_EXIT_CODE_EPERM_ERROR_SENDING_SIGKILL = 4
SHUTDOWN_EXIT_CODE_SIGKILL_FAILED_TO_KILL_EDENFS = 5


def do_shutdown(args):
    config = create_config(args)
    try:
        with config.get_thrift_client() as client:
            pid = client.getPid()
            # Ask the client to shutdown
            client.shutdown()
    except EdenNotRunningError:
        print_stderr('error: edenfs is not running')
        return SHUTDOWN_EXIT_CODE_NOT_RUNNING_ERROR

    if args.timeout == 0:
        print_stderr('Sent async shutdown request to edenfs.')
        return SHUTDOWN_EXIT_CODE_REQUESTED_SHUTDOWN

    # Wait until the process exits on its own.
    def eden_exited():
        try:
            os.kill(pid, 0)
        except OSError as ex:
            if ex.errno == errno.ESRCH:
                # The process has exited
                return True
            # EPERM is okay (and means the process is still running),
            # anything else is unexpected
            elif ex.errno != errno.EPERM:
                raise
        # Still running
        return None

    try:
        util.poll_until(eden_exited, timeout=args.timeout)
        print_stderr('edenfs exited cleanly.')
        return SHUTDOWN_EXIT_CODE_NORMAL
    except util.TimeoutError:
        pass

    # client.shutdown() failed to terminate Eden within the specified timeout.
    # Take a more aggressive approach by sending SIGKILL.
    print_stderr(
        'error: sent shutdown request, but edenfs did not exit '
        'within {} seconds. Attempting SIGKILL.', args.timeout
    )
    try:
        os.kill(pid, signal.SIGKILL)
    except OSError as ex:
        if ex.errno == errno.ESRCH:
            print_stderr('{} exited before SIGKILL was received.', pid)
            return SHUTDOWN_EXIT_CODE_NORMAL
        elif ex.errno == errno.EPERM:
            print_stderr(
                'Received EPERM when sending SIGKILL. '
                'Perhaps we failed to drop root privileges properly?')
            return SHUTDOWN_EXIT_CODE_EPERM_ERROR_SENDING_SIGKILL
        else:
            raise

    sigkill_timeout_seconds = 5
    try:
        util.poll_until(eden_exited, timeout=sigkill_timeout_seconds)
        print_stderr('Process {} was killed after sending SIGKILL.', pid)
        return SHUTDOWN_EXIT_CODE_TERMINATED_VIA_SIGKILL
    except util.TimeoutError:
        print_stderr(
            'Process {} did not terminate within {} seconds of '
            'sending SIGKILL.', pid, sigkill_timeout_seconds
        )
        return SHUTDOWN_EXIT_CODE_SIGKILL_FAILED_TO_KILL_EDENFS


def create_parser():
    '''Returns a parser and its immediate subparsers.'''
    parser = argparse.ArgumentParser(description='Manage Eden clients.')
    parser.add_argument(
        '--config-dir',
        help='Path to directory where client data is stored.')
    parser.add_argument(
        '--etc-eden-dir',
        help='Path to directory that holds the system configuration files.')
    parser.add_argument(
        '--home-dir',
        help='Path to directory where .edenrc config file is stored.')
    subparsers = parser.add_subparsers(dest='subparser_name')

    # Please add the subparsers in alphabetical order because that is the order
    # in which they are displayed when the user runs --help.
    checkout_parser = subparsers.add_parser(
        'checkout', help='Check out an alternative snapshot hash.')
    checkout_parser.add_argument('--client', '-c',
                                 default=None,
                                 help='Name of the mounted client')
    checkout_parser.add_argument('snapshot', help='Snapshot hash to check out')
    checkout_parser.set_defaults(func=do_checkout)

    clone_parser = subparsers.add_parser(
        'clone', help='Create a clone of a specific repo')
    clone_parser.add_argument(
        'repo', help='Name of repository to clone')
    clone_parser.add_argument(
        'path', help='Path where the client should be mounted')
    clone_parser.add_argument(
        '--snapshot', '-s', type=str, help='Snapshot id of revision')
    clone_parser.set_defaults(func=do_clone)

    config_parser = subparsers.add_parser(
        'config', help='Query Eden configuration')
    config_parser.add_argument(
        '--get', help='Name of value to get')
    config_parser.set_defaults(func=do_config)

    daemon_parser = subparsers.add_parser(
        'daemon', help='Run the edenfs daemon')
    daemon_parser.add_argument(
        '--daemon-binary',
        help='Path to the binary for the Eden daemon.')
    daemon_parser.add_argument(
        '--foreground', '-F', action='store_true',
        help='Run eden in the foreground, rather than daemonizing')
    daemon_parser.add_argument(
        '--gdb', '-g', action='store_true', help='Run under gdb')
    daemon_parser.add_argument(
        '--gdb-arg', action='append', default=[],
        help='Extra arguments to pass to gdb')
    daemon_parser.add_argument(
        '--strace', '-s',
        metavar='FILE',
        help='Run eden under strace, and write strace output to FILE')
    daemon_parser.add_argument(
        'edenfs_args', nargs=argparse.REMAINDER,
        help='Any extra arguments after an "--" argument will be passed to the '
        'edenfs daemon.')
    daemon_parser.set_defaults(func=do_daemon)

    health_parser = subparsers.add_parser(
        'health', help='Check the health of the Eden service')
    health_parser.set_defaults(func=do_health)

    help_parser = subparsers.add_parser(
        'help', help='Display help information about Eden.')
    help_parser.set_defaults(func=do_help)
    help_parser.add_argument('args', nargs='*')

    info_parser = subparsers.add_parser(
        'info', help='Get details about a client.')
    info_parser.add_argument(
        'client',
        default=None,
        nargs='?',
        help='Name of the client')
    info_parser.set_defaults(func=do_info)

    list_parser = subparsers.add_parser(
        'list', help='List available clients')
    list_parser.set_defaults(func=do_list)

    repository_parser = subparsers.add_parser(
        'repository', help='List all repositories')
    repository_parser.add_argument(
        'name', nargs='?', default=None, help='Name of the client to mount')
    repository_parser.add_argument(
        'path',
        nargs='?',
        default=None,
        help='Path to the repository to import')
    repository_parser.add_argument(
        '--with-buck', '-b', action='store_true',
        help='Client should create a bind mount for buck-out/.')
    repository_parser.set_defaults(func=do_repository)

    shutdown_parser = subparsers.add_parser(
        'shutdown', help='Shutdown the daemon')
    shutdown_parser.add_argument(
        '-t', '--timeout', type=float, default=15.0,
        help='Wait up to TIMEOUT seconds for the daemon to exit '
        '(default=%(default)s). If it does not exit within the timeout, then '
        'SIGKILL will be sent. If timeout is 0, then do not wait at all '
        'and do not send SIGKILL.')
    shutdown_parser.set_defaults(func=do_shutdown)

    mount_parser = subparsers.add_parser(
        'mount', help='Remount an existing client (for instance, after it was '
        'unmounted with "unmount -n")')
    mount_parser.add_argument(
        'path', help='The client mount path')
    mount_parser.set_defaults(func=do_mount)

    unmount_parser = subparsers.add_parser(
        'unmount', help='Unmount a specific client')
    unmount_parser.add_argument(
        '-n', '--no-forget',
        action='store_true',
        help='Only unmount the client, without forgetting about its '
        'configuration.  The client can be re-mounted later using the mount '
        'command.')
    unmount_parser.add_argument(
        'path', help='Path where client should be unmounted from')
    unmount_parser.set_defaults(func=do_unmount)

    # We intentionally do not specify a help option for debug, so it
    # does not show up in the --help output.  (It appears that add_parser()
    # unfortunately does not honor help=argparse.SUPPRESS the same way
    # that add_argument() does.  Not specifying help at all suppresses the
    # output instead.)
    debug_parser = subparsers.add_parser('debug')
    debug_mod.setup_argparse(debug_parser)

    rage_parser = subparsers.add_parser(
        'rage', help='Prints the diagnostic information about eden')
    rage_parser.set_defaults(func=do_rage)

    stats_parser = subparsers.add_parser(
        'stats', help='Prints statistics information for eden'
    )
    stats_mod.setup_argparse(stats_parser)
    stats_parser.set_defaults(func=do_stats)

    return parser, subparsers


def main():
    parser, subparsers = create_parser()
    args = parser.parse_args()
    if args.subparser_name == 'help' or getattr(args, 'func', None) is None:
        retcode = do_help(args, parser, subparsers)
    else:
        retcode = args.func(args)
    return retcode


def print_stderr(message, *args, **kwargs):
    '''Prints the message to stderr.'''
    if args or kwargs:
        message = message.format(*args, **kwargs)
    print(message, file=sys.stderr)


def normalize_path_arg(path_arg, may_need_tilde_expansion=False):
    '''Normalizes a path by using os.path.realpath().

    Note that this function is expected to be used with command-line arguments.
    If the argument comes from a config file or GUI where tilde expansion is not
    done by the shell, then may_need_tilde_expansion=True should be specified.
    '''
    if path_arg:
        if may_need_tilde_expansion:
            path_arg = os.path.expanduser(path_arg)

        # Use the canonical version of the path.
        path_arg = os.path.realpath(path_arg)
    return path_arg


if __name__ == '__main__':
    retcode = main()
    sys.exit(retcode)
