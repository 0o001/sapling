#!/usr/bin/env python3
#
# Copyright (c) 2004-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import os

from . import config as config_mod
from . import util

# Relative to the user's $HOME/%USERPROFILE% directory.
# TODO: This value should be .eden outside of Facebook devservers.
DEFAULT_CONFIG_DIR = 'local/.eden'

# Environment variable that can be used instead of specifying --config-dir.
CONFIG_DIR_ENVIRONMENT_VARIABLE = 'EDEN_CONFIG_DIR'


def find_default_config_dir(home_dir):
    '''Returns the path to default Eden config directory.

    If the environment variable $EDEN_CONFIG_DIR is set, it takes precedence
    over the default, which is "$HOME/.eden".

    Note that the path is not guaranteed to correspond to an existing directory.
    '''
    config_dir = os.getenv(CONFIG_DIR_ENVIRONMENT_VARIABLE)
    if config_dir:
        return config_dir

    return os.path.join(home_dir, DEFAULT_CONFIG_DIR)


def create_config(args):
    home_dir = args.home_dir or util.get_home_dir()
    config = args.config_dir or find_default_config_dir(home_dir)
    return config_mod.Config(config, args.etc_eden_dir, home_dir)
