#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import errno
import os
import pwd
import subprocess
import sys
import time
import typing
from typing import Any, Callable, Optional, Tuple, TypeVar


class TimeoutError(Exception):
    pass


T = TypeVar('T')


def poll_until(
    function: Callable[[], Optional[T]],
    timeout: float,
    interval: float=0.2,
    timeout_ex: Optional[Exception]=None
) -> T:
    '''
    Call the specified function repeatedly until it returns non-None.
    Returns the function result.

    Sleep 'interval' seconds between calls.  If 'timeout' seconds passes
    before the function returns a non-None result, raise an exception.
    If a 'timeout_ex' argument is supplied, that exception object is
    raised, otherwise a TimeoutError is raised.
    '''
    end_time = time.time() + timeout
    while True:
        result = function()
        if result is not None:
            return result

        if time.time() >= end_time:
            if timeout_ex is not None:
                raise timeout_ex
            raise TimeoutError('timed out waiting on function {}'.format(
                function.__name__))

        time.sleep(interval)


def get_home_dir() -> str:
    home_dir = None
    if os.name == 'nt':
        home_dir = os.getenv('USERPROFILE')
    else:
        home_dir = os.getenv('HOME')
    if not home_dir:
        home_dir = pwd.getpwuid(os.getuid()).pw_dir
    return home_dir


def mkdir_p(path: str) -> str:
    '''Performs `mkdir -p <path>` and returns the path.'''
    try:
        os.makedirs(path)
    except OSError as e:
        if e.errno != errno.EEXIST:
            raise
    return path


def is_git_dir(path: str) -> bool:
    return (os.path.isdir(os.path.join(path, 'objects')) and
            os.path.isdir(os.path.join(path, 'refs')) and
            os.path.exists(os.path.join(path, 'HEAD')))


def get_git_dir(path: str) -> Optional[str]:
    '''
    If path points to a git repository, return the path to the repository .git
    directory.  Otherwise, if the path is not a git repository, return None.
    '''
    path = os.path.realpath(path)
    if path.endswith('.git') and is_git_dir(path):
        return path

    git_subdir = os.path.join(path, '.git')
    if is_git_dir(git_subdir):
        return git_subdir

    return None


def get_git_commit(git_dir: str) -> str:
    cmd = ['git', 'rev-parse', 'HEAD']
    out = typing.cast(bytes, subprocess.check_output(cmd, cwd=git_dir))
    return out.strip().decode('utf-8', errors='surrogateescape')


def get_hg_repo(path: str) -> Optional[str]:
    '''
    If path points to a mercurial repository, return a normalized path to the
    repository root.  Otherwise, if path is not a mercurial repository, return
    None.
    '''
    repo_path = os.path.realpath(path)
    hg_dir = os.path.join(repo_path, '.hg')
    if not os.path.isdir(hg_dir):
        return None

    # Check to see if this is a shared working directory from another
    # repository
    try:
        with open(os.path.join(hg_dir, 'sharedpath'), 'r') as f:
            hg_dir = f.readline().rstrip('\n')
            hg_dir = os.path.realpath(hg_dir)
            repo_path = os.path.dirname(hg_dir)
    except EnvironmentError as ex:
        if ex.errno != errno.ENOENT:
            raise

    if not os.path.isdir(os.path.join(hg_dir, 'store')):
        return None

    return repo_path


def get_hg_commit(repo: str) -> str:
    env = os.environ.copy()
    env['HGPLAIN'] = '1'
    cmd = ['hg', '--cwd', repo, 'log', '-T{node}', '-r.']
    out = typing.cast(bytes, subprocess.check_output(cmd, env=env))
    return out.decode('utf-8', errors='strict')


def get_repo_source_and_type(path: str) -> Tuple[str, Optional[str]]:
    repo_source = ''
    repo_type = None
    git_dir = get_git_dir(path)
    if git_dir:
        repo_source = git_dir
        repo_type = 'git'
    else:
        hg_repo = get_hg_repo(path)
        if hg_repo:
            repo_source = hg_repo
            repo_type = 'hg'
    return (repo_source, repo_type)


def print_stderr(message: str, *args: Any, **kwargs: Any) -> None:
    '''Prints the message to stderr.'''
    if args or kwargs:
        message = message.format(*args, **kwargs)
    print(message, file=sys.stderr)
