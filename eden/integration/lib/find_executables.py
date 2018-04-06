#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

'''
These utilities are only expected to work if `sys.argv[0]` is an executable
being run in buck-out.
'''

import os
import sys
from typing import Callable, Dict, List, Optional, Type


class cached_property(object):
    def __init__(self, find: Callable[['FindExeClass'], str]) -> None:
        self.name: Optional[str] = None
        self.find = find

    def __get__(self, instance: 'FindExeClass',
                owner: Type['FindExeClass']) -> str:
        assert self.name is not None
        result = instance._cache.get(self.name, None)
        if result is None:
            result = self.find(instance)
            instance._cache[self.name] = result
        return result

    def __set_name__(self, owner: Type['FindExeClass'], name: str) -> None:
        self.name = name


class FindExeClass(object):
    def __init__(self) -> None:
        self._cache: Dict[str, str] = {}

    @property
    def BUCK_OUT(self) -> str:
        if not hasattr(self, '_BUCK_OUT'):
            self._find_repo_root_and_buck_out()
        return self._BUCK_OUT

    @property
    def REPO_ROOT(self) -> str:
        if not hasattr(self, '_REPO_ROOT'):
            self._find_repo_root_and_buck_out()
        return self._REPO_ROOT

    @cached_property
    def EDEN_CLI(self) -> str:
        return self._find_exe(
            'eden CLI',
            env='EDENFS_CLI_PATH',
            candidates=[os.path.join(self.BUCK_OUT, 'gen/eden/cli/cli.par')]
        )

    @cached_property
    def EDEN_DAEMON(self) -> str:
        edenfs_suffix = os.environ.get('EDENFS_SUFFIX', '')
        edenfs = os.path.join(
            self.BUCK_OUT, 'gen/eden/fs/service/edenfs%s' % edenfs_suffix
        )
        return self._find_exe(
            'edenfs daemon',
            env='EDENFS_SERVER_PATH',
            candidates=[edenfs],
        )

    @cached_property
    def EDEN_HG_IMPORT_HELPER(self) -> str:
        return self._find_exe(
            'hg_import_helper',
            env='EDENFS_HG_IMPORT_HELPER',
            candidates=[
                os.path.join(
                    self.BUCK_OUT, 'gen/eden/fs/store/hg/hg_import_helper.par'
                ),
                os.path.join(
                    self.REPO_ROOT, 'eden/fs/store/hg/hg_import_helper.py'
                ),
            ]
        )

    @cached_property
    def FSATTR(self) -> str:
        return self._find_exe(
            'fsattr',
            env='EDENFS_FSATTR_BIN',
            candidates=[
                os.path.join(self.BUCK_OUT, 'gen/eden/integration/fsattr')
            ]
        )

    def _find_exe(self, name, env, candidates):
        if env is not None:
            path = os.environ.get(env)
            if path and not os.access(path, os.X_OK):
                raise Exception(f'unable to find {name}: specified as {path!r} '
                                f'by ${env}, but not available there')

        for path in candidates:
            if os.access(path, os.X_OK):
                return path

        raise Exception(f'unable to find {name}')

    def _find_repo_root_and_buck_out(self) -> None:
        '''Finds the paths to buck-out and the repo root.

        Note that the path to buck-out may not be "buck-out" under the repo
        root because Buck could have been run with `buck --config
        project.buck_out` and sys.argv[0] could be the realpath rather than the
        symlink under buck-out.

        TODO: We will have to use a different heuristic for open source builds
        that build with CMake. (Ultimately, we would prefer to build them with
        Buck.)
        '''
        executable = sys.argv[0]
        path = os.path.dirname(os.path.abspath(executable))
        while True:
            parent = os.path.dirname(path)
            parent_basename = os.path.basename(parent)
            if parent_basename == 'buck-out':
                self._REPO_ROOT = os.path.dirname(parent)
                if os.path.basename(path) in ['bin', 'gen']:
                    self._BUCK_OUT = parent
                else:
                    self._BUCK_OUT = path
                return
            if parent == path:
                raise Exception(
                    'Path to repo root not found from %s' % executable
                )
            path = parent


# The main FindExe singleton
FindExe = FindExeClass()
