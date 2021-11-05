# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from edenscm.mercurial import error, registrar
from edenscm.mercurial.i18n import _

from . import createremote, update, show

cmdtable = {}
command = registrar.command(cmdtable)


@command("snapshot", [], "SUBCOMMAND ...")
def snapshot(ui, repo, **opts):
    """create and share snapshots with uncommitted changes"""

    raise error.Abort(
        "you need to specify a subcommand (run with --help to see a list of subcommands)"
    )


subcmd = snapshot.subcommand(
    categories=[
        ("Manage snapshots", ["create", "update"]),
        ("Query snapshots", ["show"]),
    ]
)


@subcmd(
    "createremote|create",
    [
        (
            "L",
            "lifetime",
            "",
            _(
                "how long the snapshot should last for, seconds to days supported (e.g. 60s, 90d, 1h30m)"
            ),
            _("LIFETIME"),
        )
    ],
)
def createremotecmd(*args, **kwargs):
    """upload to the server a snapshot of the current uncommitted changes"""
    createremote.createremote(*args, **kwargs)


@subcmd(
    "update|restore|checkout|co|up",
    [
        (
            "C",
            "clean",
            None,
            _("discard uncommitted changes and untracked files (no backup)"),
        )
    ],
    _("ID"),
)
def updatecmd(*args, **kwargs):
    """download a previously created snapshot and update working copy to its state"""
    update.update(*args, **kwargs)


@subcmd(
    "show|info",
    [("", "json", None, _("Print info about snapshot in json format"))],
    _("ID"),
)
def showcmd(*args, **kwargs):
    """gather information about the snapshot"""
    show.show(*args, **kwargs)
