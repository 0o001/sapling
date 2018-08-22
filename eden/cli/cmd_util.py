#!/usr/bin/env python3
#
# Copyright (c) 2004-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import argparse
import os
from pathlib import Path
from typing import Optional, Tuple, Union

from . import config as config_mod, subcmd as subcmd_mod
from .config import EdenCheckout, EdenInstance


def get_eden_instance(args: argparse.Namespace) -> EdenInstance:
    return EdenInstance(
        args.config_dir, etc_eden_dir=args.etc_eden_dir, home_dir=args.home_dir
    )


def find_checkout(
    args: argparse.Namespace, path: Union[Path, str]
) -> Tuple[EdenInstance, Optional[EdenCheckout], Optional[Path]]:
    if path is None:
        path = os.getcwd()
    return config_mod.find_eden(
        path,
        etc_eden_dir=args.etc_eden_dir,
        home_dir=args.home_dir,
        state_dir=args.config_dir,
    )


def require_checkout(
    args: argparse.Namespace, path: Union[Path, str]
) -> Tuple[EdenInstance, EdenCheckout, Path]:
    instance, checkout, rel_path = find_checkout(args, path)
    if checkout is None:
        raise subcmd_mod.CmdError(f"no Eden checkout found at {path}")
    assert rel_path is not None
    return instance, checkout, rel_path
