# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from edenscm.mercurial import error
from edenscm.mercurial.i18n import _

from .createremote import parsemaxuntracked
from .latest import _isworkingcopy


def cmd(ui, repo, csid=None, **opts):
    if csid is None:
        raise error.CommandError("snapshot isworkingcopy", _("missing snapshot id"))
    try:
        snapshot = repo.edenapi.fetchsnapshot(
            {
                "cs_id": bytes.fromhex(csid),
            },
        )
    except Exception:
        raise error.Abort(_("snapshot doesn't exist"))
    else:
        maxuntrackedsize = parsemaxuntracked(opts)
        iswc, reason = _isworkingcopy(ui, repo, snapshot, maxuntrackedsize)
        if iswc:
            if not ui.plain():
                ui.status(_("snapshot is the working copy\n"))
        else:
            raise error.Abort(_("snapshot is not the working copy: {}").format(reason))
