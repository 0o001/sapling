#!/usr/bin/env python3
#
# Copyright (c) 2016-present, Facebook, Inc.
# All rights reserved.
#
# This source code is licensed under the BSD-style license found in the
# LICENSE file in the root directory of this source tree. An additional grant
# of patent rights can be found in the PATENTS file in the same directory.

import glob
import os
import subprocess
from typing import List


def find_buck_projects_in_repo(path: str) -> List[str]:
    # This is a largely Facebook specific way to discover the likely
    # buck project locations in our repos.
    # While fbsource has a top level buckconfig, we don't really use
    # it in our projects today.  Instead, our projects tend to have
    # their own configuration files one level down.  This glob()
    # finds those directories for us.
    buck_configs = glob.glob(f"{path}/*/.buckconfig")
    projects = [os.path.dirname(config) for config in buck_configs]
    if os.path.isfile(f"{path}/.buckconfig"):
        projects.append(path)
    return projects


def stop_buckd_for_path(path: str) -> None:
    print(f"Stopping buck in {path}...")
    subprocess.run(
        # Using BUCKVERSION=last here to avoid triggering a download
        # of a new version of buck just to kill off buck.
        # This is specific to Facebook's deployment of buck, and has
        # no impact on the behavior of the opensource buck executable.
        ["env", "BUCKVERSION=last", "buck", "kill"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        cwd=path,
    )


def stop_buckd_for_repo(path: str) -> None:
    """Stop the major buckd instances that are likely to be running for path"""
    for project in find_buck_projects_in_repo(path):
        stop_buckd_for_path(project)


def buck_clean_repo(path: str) -> None:
    for project in find_buck_projects_in_repo(path):
        print(f"Cleaning buck in {project}...")
        subprocess.run(
            # Using BUCKVERSION=last here to avoid triggering a download
            # of a new version of buck just to remove some dirs
            # This is specific to Facebook's deployment of buck, and has
            # no impact on the behavior of the opensource buck executable.
            ["env", "NO_BUCKD=true", "BUCKVERSION=last", "buck", "clean"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            cwd=project,
        )
