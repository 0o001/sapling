# split.py - split a changeset into smaller parts
#
# Copyright 2011 Peter Arrenbrecht <peter.arrenbrecht@gmail.com>
#                Logilab SA        <contact@logilab.fr>
#                Pierre-Yves David <pierre-yves.david@ens-lyon.org>
#                Patrick Mezard <patrick@mezard.eu>
# Copyright 2017 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

from mercurial import (
    bookmarks,
    cmdutil,
    commands,
    error,
    hg,
    hintutil,
    lock as lockmod,
    obsolete,
    registrar,
    scmutil,
)
from mercurial.i18n import _

from . import common
from ..extlib.phabricator import diffprops


cmdtable = {}
command = registrar.command(cmdtable)


@command(
    "^split",
    [
        ("r", "rev", [], _("revision to split")),
        ("", "no-rebase", False, _("don't rebase descendants after split")),
    ]
    + commands.commitopts
    + commands.commitopts2,
    _("hg split [OPTION]... [-r] [REV]"),
)
def split(ui, repo, *revs, **opts):
    """split a changeset into smaller changesets

    Prompt for hunks to be selected until exhausted. Each selection of hunks
    will form a separate changeset, in order from parent to child: the first
    selection will form the first changeset, the second selection will form
    the second changeset, and so on.

    Operates on the current revision by default. Use --rev to split a given
    changeset instead.
    """
    tr = wlock = lock = None
    newcommits = []

    revarg = (list(revs) + opts.get("rev")) or ["."]
    if len(revarg) != 1:
        msg = _("more than one revset is given")
        hnt = _("use either `hg split <rs>` or `hg split --rev <rs>`, not both")
        raise error.Abort(msg, hint=hnt)

    rev = scmutil.revsingle(repo, revarg[0])
    if opts.get("no_rebase"):
        torebase = ()
    else:
        torebase = repo.revs("descendants(%d) - (%d)", rev, rev)
    try:
        wlock = repo.wlock()
        lock = repo.lock()
        cmdutil.bailifchanged(repo)
        if torebase:
            cmdutil.checkunfinished(repo)
        tr = repo.transaction("split")
        ctx = repo[rev]
        r = ctx.rev()
        disallowunstable = not obsolete.isenabled(repo, obsolete.allowunstableopt)
        if disallowunstable:
            # XXX We should check head revs
            if repo.revs("(%d::) - %d", rev, rev):
                raise error.Abort(_("cannot split commit: %s not a head") % ctx)

        if len(ctx.parents()) > 1:
            raise error.Abort(_("cannot split merge commits"))
        prev = ctx.p1()
        bmupdate = common.bookmarksupdater(repo, ctx.node(), tr)
        bookactive = repo._activebookmark
        if bookactive is not None:
            repo.ui.status(_("(leaving bookmark %s)\n") % repo._activebookmark)
        bookmarks.deactivate(repo)
        hg.update(repo, prev)

        commands.revert(ui, repo, rev=r, all=True)

        def haschanges():
            modified, added, removed, deleted = repo.status()[:4]
            return modified or added or removed or deleted

        msg = (
            "HG: This is the original pre-split commit message. "
            "Edit it as appropriate.\n\n"
        )
        msg += ctx.description()
        opts["message"] = msg
        opts["edit"] = True
        while haschanges():
            pats = ()
            cmdutil.dorecord(
                ui,
                repo,
                commands.commit,
                "commit",
                False,
                cmdutil.recordfilter,
                *pats,
                **opts
            )
            # TODO: Does no seem like the best way to do this
            # We should make dorecord return the newly created commit
            newcommits.append(repo["."])
            if haschanges():
                if ui.prompt("Done splitting? [yN]", default="n") == "y":
                    commands.commit(ui, repo, **opts)
                    newcommits.append(repo["."])
                    break
            else:
                ui.status(_("no more change to split\n"))

        if newcommits:
            phabdiffs = {}
            for c in newcommits:
                phabdiff = diffprops.parserevfromcommitmsg(repo[c].description())
                if phabdiff:
                    phabdiffs.setdefault(phabdiff, []).append(c)
            if any(len(commits) > 1 for commits in phabdiffs.values()):
                hintutil.trigger(
                    "split-phabricator", ui.config("split", "phabricatoradvice")
                )

            tip = repo[newcommits[-1]]
            bmupdate(tip.node())
            if bookactive is not None:
                bookmarks.activate(repo, bookactive)
            obsolete.createmarkers(repo, [(repo[r], newcommits)], operation="split")

            if torebase:
                common.restackonce(ui, repo, tip.rev())
        tr.close()
    finally:
        lockmod.release(tr, lock, wlock)
