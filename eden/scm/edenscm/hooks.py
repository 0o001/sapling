# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

"""predefined hooks"""

from .mercurial import util


def backgroundfsync(ui, repo, hooktype, **kwargs):
    """run fsync in background

    Example config::

        [hooks]
        postwritecommand.fsync = python:edenscm.hooks.backgroundfsync
    """
    if not repo:
        return
    util.spawndetached(util.gethgcmd() + ["debugfsync"], cwd=repo.svfs.join(""))
