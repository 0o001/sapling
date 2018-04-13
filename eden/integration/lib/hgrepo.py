#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import configparser
import datetime
import json
import logging
import os
import subprocess
import sys
import tempfile
import textwrap
import typing
from typing import Any, Dict, List, Optional

from . import repobase
from .error import CommandError
from .find_executables import FindExe


class HgError(CommandError):
    pass


class HgRepository(repobase.Repository):
    def __init__(self, path: str, system_hgrc: Optional[str] = None) -> None:
        '''
        If hgrc is specified, it will be used as the value of the HGRCPATH
        environment variable when `hg` is run.
        '''
        super().__init__(path)
        self.hg_environment = os.environ.copy()
        # Drop any environment variables starting with 'HG'
        # to ensure the user's environment does not affect the tests
        self.hg_environment = dict((k, v) for k, v in os.environ.items()
                                   if not k.startswith('HG'))
        self.hg_environment['HGPLAIN'] = '1'
        # Set HGRCPATH to make sure we aren't affected by the local system's
        # mercurial settings from /etc/mercurial/
        if system_hgrc:
            self.hg_environment['HGRCPATH'] = system_hgrc
        else:
            self.hg_environment['HGRCPATH'] = ''
        self.hg_bin = FindExe.HG

    @classmethod
    def get_system_hgrc_contents(cls) -> str:
        hgrc_path = 'scm/hg/fb/staticfiles/etc/mercurial'
        contents = textwrap.dedent(
            '''
            %include {repo_root}/{hgrc_path}/facebook.rc
            %include {repo_root}/{hgrc_path}/tier-specific/posix.rc
            %include {repo_root}/{hgrc_path}/tier-specific/client.rc

            # Override ui.merge to make sure it does not get set
            # to something that tries to prompt for user input.
            [ui]
            merge = :merge
            '''
        ).format(
            repo_root=FindExe.REPO_ROOT, hgrc_path=hgrc_path
        )
        return contents

    def run_hg(
        self,
        *args: str,
        encoding: str = 'utf-8',
        stdout: Any = subprocess.PIPE,
        stderr: Any = subprocess.PIPE,
        input: Optional[str] = None,
        hgeditor: Optional[str] = None,
        cwd: Optional[str] = None,
        check: bool = True
    ) -> subprocess.CompletedProcess:
        cmd = [self.hg_bin] + list(args)
        env = self.hg_environment
        if hgeditor is not None:
            env = dict(env)
            env['HGEDITOR'] = hgeditor

        input_bytes = None
        if input is not None:
            input_bytes = input.encode(encoding)

        if cwd is None:
            cwd = self.path
        try:
            return subprocess.run(
                cmd,
                stdout=stdout,
                stderr=stderr,
                input=input_bytes,
                check=check,
                cwd=cwd,
                env=env
            )
        except subprocess.CalledProcessError as ex:
            raise HgError(ex) from ex

    def hg(
        self,
        *args: str,
        encoding: str = 'utf-8',
        stdout: Any = subprocess.PIPE,
        stderr: Any = subprocess.PIPE,
        input: Optional[str] = None,
        hgeditor: Optional[str] = None,
        cwd: Optional[str] = None,
        check: bool = True
    ) -> Optional[str]:
        completed_process = self.run_hg(
            *args,
            encoding=encoding,
            stdout=stdout,
            stderr=stderr,
            input=input,
            hgeditor=hgeditor,
            cwd=cwd,
            check=check
        )
        if completed_process.stdout is not None:
            return typing.cast(str, completed_process.stdout.decode(encoding))
        else:
            return None

    def init(self, hgrc: Optional[configparser.ConfigParser] = None) -> None:
        '''
        Initialize a new hg repository by running 'hg init'

        The hgrc parameter may be a configparser.ConfigParser() object
        describing configuration settings that should be added to the
        repository's .hg/hgrc file.
        '''
        self.hg('init')
        if hgrc is not None:
            self.write_hgrc(hgrc)

    def write_hgrc(self, hgrc: configparser.ConfigParser) -> None:
        hgrc_path = os.path.join(self.path, '.hg', 'hgrc')
        with open(hgrc_path, 'a') as f:
            hgrc.write(f)

    def get_type(self) -> str:
        return 'hg'

    def get_head_hash(self) -> str:
        return self.hg('log', '-r.', '-T{node}')

    def get_canonical_root(self) -> str:
        return self.path

    def add_files(self, paths: List[str]) -> None:
        # add_files() may be called for files that are already tracked.
        # hg will print a warning, but this is fine.
        self.hg('add', *paths)

    def remove_files(self, paths: List[str], force: bool = False) -> None:
        if force:
            self.hg('remove', '--force', *paths)
        else:
            self.hg('remove', *paths)

    def commit(self,
               message: str,
               author_name: Optional[str]=None,
               author_email: Optional[str]=None,
               date: Optional[datetime.datetime]=None,
               amend: bool=False) -> str:
        '''
        - message Commit message to use.
        - author_name Author name to use: defaults to self.author_name.
        - author_email Author email to use: defaults to self.author_email.
        - date datetime.datetime to use for the commit. Defaults to
          self.get_commit_time().
        - amend If true, adds the `--amend` argument.
        '''
        if author_name is None:
            author_name = self.author_name
        if author_email is None:
            author_email = self.author_email
        if date is None:
            date = self.get_commit_time()
        # Mercurial's internal format of <unix_timestamp> <timezone>
        date_str = '{} 0'.format(int(date.timestamp()))

        user_config = 'ui.username={} <{}>'.format(author_name, author_email)

        with tempfile.NamedTemporaryFile(prefix='eden_commit_msg.',
                                         mode='w',
                                         encoding='utf-8') as msgf:
            msgf.write(message)
            msgf.flush()

            args = [
                'commit',
                '--config', user_config,
                '--date', date_str,
                '--logfile', msgf.name,
            ]
            if amend:
                args.append('--amend')

            # Do not capture stdout or stderr when running "hg commit"
            # This allows its output to show up in the test logs.
            self.hg(*args, stdout=None, stderr=None)

        # Get the commit ID and return it
        return self.hg('log', '-T{node}', '-r.')

    def log(self, template: str = '{node}', revset: str = '::.') -> List[str]:
        '''Runs `hg log` with the specified template and revset.

        Returns the log output, as a list with one entry per commit.'''
        # Append a separator to the template so we can split up the entries
        # afterwards.  Use a slightly more complex string rather than just a
        # single nul byte, just in case the caller uses internal nuls in their
        # template to split fields.
        escaped_delimiter = r'\0-+-\0'
        delimiter = '\0-+-\0'
        assert escaped_delimiter not in template
        template += escaped_delimiter
        output = self.hg('log', '-T', template, '-r', revset)
        return output.split(delimiter)[:-1]

    def journal(self) -> List[Dict[str, Any]]:
        output = self.hg('journal', '-T', 'json')
        return json.loads(output)

    def status(self) -> str:
        '''Returns the output of `hg status` as a string.'''
        return self.hg('status')

    def update(
        self, rev: str, clean: bool = False, merge: bool = False
    ) -> None:
        args = ['update']
        if clean:
            args.append('--clean')
        if merge:
            args.append('--merge')
        args.append(rev)
        self.hg(*args)

    def reset(self, rev: str, keep: bool = True) -> None:
        if keep:
            args = ['reset', '--keep', rev]
        else:
            args = ['reset', rev]
        self.hg(*args, stdout=None, stderr=None)
