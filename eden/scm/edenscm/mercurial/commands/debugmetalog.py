# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

from __future__ import absolute_import

import collections
from typing import Set, Tuple

from .. import cmdutil, graphmod, phases, pycompat, util
from ..i18n import _
from ..node import bin, hex, short
from .cmdtable import command


@command("debugmetalog", [("t", "time-range", [], _("select time range"), _("TIME"))])
def debugmetalog(ui, repo, **opts):
    # type: (...) -> None
    """show changes in commit graph over time"""

    matchdatefuncs = []
    for timerange in opts.get("time_range") or []:
        matchdate = util.matchdate(timerange)
        matchdatefuncs.append(matchdate)

    metalog = repo.svfs.metalog
    metalogpath = repo.svfs.join("metalog")
    roots = metalog.listroots(metalogpath)
    if matchdatefuncs:
        roots = [
            r
            for r in roots
            if any(
                m(metalog.__class__(metalogpath, r).timestamp()) for m in matchdatefuncs
            )
        ]

    now, tzoffset = util.parsedate("now")
    nodenamesdict = collections.defaultdict(list)  # {node: [desc]}
    currentnodenames = set()  # {(node, name)}
    for root in roots:
        meta = metalog.__class__(metalogpath, root)
        timestamp = meta.timestamp()
        desc = meta.message()
        date = util.datestr((timestamp, tzoffset), "%Y-%m-%d %H:%M:%S %1%2")
        nextnodenames = parsenodenames(meta)
        first = not nodenamesdict
        for node, name in nextnodenames - currentnodenames:
            if not first:
                name = ui.label(name, "diff.inserted")
                nodenamesdict[node].append("%s: %s (added by %s)" % (date, name, desc))
        for node, name in currentnodenames - nextnodenames:
            name = ui.label(name, "diff.deleted")
            nodenamesdict[node].append("%s: %s (removed by %s)" % (date, name, desc))
        currentnodenames = nextnodenames

    revdag = graphmod.dagwalker(
        repo,
        repo.revs(
            "sort(%ln + p1((not public()) & (::((not public()) & %ln))), -rev)",
            nodenamesdict.keys(),
            nodenamesdict.keys(),
        ),
    )
    ui.pager("debugmetalog")
    cmdutil.rustdisplaygraph(ui, repo, revdag, displayer(nodenamesdict))


def parsenodenames(meta):
    # type: (...) -> Set[Tuple[bytes, str]]
    """Parse a metalog entry.  Return nodes and their names."""

    nodenames = set()
    for line in (meta.get("bookmarks") or b"").splitlines():
        hexnode, name = line.split(b" ", 1)
        nodenames.add((bin(hexnode), pycompat.decodeutf8(name)))

    for line in (meta.get("remotenames") or b"").splitlines():
        hexnode, typename, name = line.split(b" ", 2)
        if typename == "bookmarks":
            nodenames.add((bin(hexnode), pycompat.decodeutf8(name)))

    for hexnode in (meta.get("visibleheads") or b"").splitlines()[1:]:
        nodenames.add((bin(hexnode), "."))

    return nodenames


class displayer(object):
    """show changeset information with debugmetalog context."""

    def __init__(self, nodenamesdict):
        self.nodenamesdict = nodenamesdict
        self.hunk = {}

    def flush(self, ctx):
        pass

    def close(self):
        pass

    def show(self, ctx, copies=None, matchfn=None, hunksfilterfn=None, **props):
        ui = ctx.repo().ui
        shorthex = short(ctx.node())
        if ctx.phase() == phases.public:
            shorthex = ui.label(shorthex, "changeset.public")
            desc = ""
        else:
            shorthex = ui.label(shorthex, "changeset.draft")
            desc = ctx.description().split("\n")[0]
        content = "%s %s\n" % (shorthex, desc)
        content += "".join(
            l + "\n"
            for l in sorted(self.nodenamesdict.get(ctx.node()) or (), reverse=True)
        )
        # Keep an empty line.
        content += " "
        # The graph renderer will call self.hunk.pop(ctx.rev()) to get the
        # content.
        self.hunk[ctx.rev()] = content


@command("debugmetalogroots", [] + cmdutil.templateopts)
def debugmetalogroots(ui, repo, **opts):
    """list roots stored in metalog"""
    metalog = repo.svfs.metalog
    metalogpath = repo.svfs.join("metalog")
    roots = metalog.listroots(metalogpath)
    _now, tzoffset = util.parsedate("now")
    ui.pager("debugmetalogroots")
    fm = ui.formatter("debugmetalogroots", opts)
    # from the newest to the oldest
    for i, root in reversed(list(enumerate(roots))):
        meta = metalog.__class__(metalogpath, root)
        timestamp = meta.timestamp()
        desc = meta.message()
        shortdesc = util.ellipsis(desc, 60)
        datestr = util.datestr((timestamp, tzoffset), "%Y-%m-%d %H:%M:%S %1%2")
        hexroot = hex(root)
        fm.startitem()
        fm.write(
            "index datestr root shortdesc",
            "%5s %s %s %s\n",
            i,
            datestr,
            hexroot,
            shortdesc,
        )
        fm.data(root=hexroot, date=timestamp, desc=desc, index=i)
    fm.end()
