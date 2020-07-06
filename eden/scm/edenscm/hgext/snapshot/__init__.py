# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# snapshot - working copy snapshots

"""extension to snapshot the working copy

With this extension, Mercurial will get a set of commands
for working with snapshots of the whole working copy,
including the untracked files and unresolved merge artifacts.

A snapshot is a hidden commit which has some extra metadata attached to it.
The metadata preserves the information about the

* untracked files (?);

* missing files (!);

* aux files related to merge/rebase state.

The snapshot metadata is stored in the `.hg/store/snapshots/` directory and is synced via infinitepush and commitcloud Mercurial extensions.

Configs::

    [ui]
    # Allow to run `hg checkout` for snapshot revisions
    allow-checkout-snapshot = False

    [snapshot]
    # Sync snapshot metadata via bundle2
    enable-sync-bundle = False

    # Size of a file to make it use a separate blob
    threshold = 1K

    # The local directory to store blob file for sharing across local clones
    # If not set, the cache is disabled (default)
    usercache = /path/to/global/cache
"""

from __future__ import absolute_import

from edenscm.mercurial import (
    blobstore as blobstoremod,
    bundlerepo,
    commands,
    error,
    extensions,
    graphmod,
    hg,
    registrar,
    revsetlang,
    smartset,
    templatekw,
    util,
    visibility,
)
from edenscm.mercurial.i18n import _

from . import blobstore, bundleparts, cmds as snapshotcommands, snapshotlist


cmdtable = snapshotcommands.cmdtable

configtable = {}
configitem = registrar.configitem(configtable)
configitem("ui", "allow-checkout-snapshot", default=False)
configitem("snapshot", "enable-sync-bundle", default=False)
configitem("snapshot", "threshold", default="100B")
configitem("snapshot", "usercache", default=None)


def uisetup(ui):
    tweakorder()
    bundleparts.uisetup(ui)


def tweakorder():
    """Snapshot extension should be loaded as soon as possible
    to prevent other extensions (e.g. infinitepush)
    from accessing snapshots as if they were normal commits.
    """
    order = extensions._order
    order.remove("snapshot")
    order.insert(0, "snapshot")
    extensions._order = order


def reposetup(ui, repo):
    # Nothing to do with a remote repo
    if not repo.local():
        return

    threshold = repo.ui.configbytes("snapshot", "threshold")

    repo.svfs.options["snapshotthreshold"] = threshold
    repo.svfs.snapshotstore = blobstore.local(repo)
    if util.safehasattr(repo, "_snapshotbundlestore"):
        repo.svfs.snapshotstore = blobstoremod.unionstore(
            repo.svfs.snapshotstore, repo._snapshotbundlestore
        )
    snapshotlist.reposetup(ui, repo)


def extsetup(ui):
    extensions.wrapfunction(graphmod, "dagwalker", _dagwalker)
    extensions.wrapfunction(hg, "updaterepo", _updaterepo)
    extensions.wrapfunction(visibility.visibleheads, "_updateheads", _updateheads)
    extensions.wrapfunction(templatekw, "showgraphnode", _showgraphnode)
    templatekw.keywords["graphnode"] = templatekw.showgraphnode
    extensions.wrapfunction(
        bundlerepo.bundlerepository, "_handlebundle2part", _handlebundle2part
    )
    extensions.wrapcommand(commands.table, "update", _update)

    def wrapamend(loaded):
        if not loaded:
            return
        amend = extensions.find("amend")
        extensions.wrapfunction(amend.hide, "_dounhide", _dounhide)

    def wrapsmartlog(loaded):
        if not loaded:
            return
        smartlog = extensions.find("smartlog")
        extensions.wrapfunction(smartlog, "smartlogrevset", _smartlogrevset)
        smartlog.revsetpredicate._table["smartlog"] = smartlog.smartlogrevset

    extensions.afterloaded("amend", wrapamend)
    extensions.afterloaded("smartlog", wrapsmartlog)


def _dagwalker(orig, repo, revs):
    return orig(repo, revs)


def _updaterepo(orig, repo, node, overwrite, **opts):
    """prevents the repo from updating onto a snapshot node
    """
    allowsnapshots = repo.ui.configbool("ui", "allow-checkout-snapshot")
    unfi = repo
    if not allowsnapshots and node in unfi:
        ctx = unfi[node]
        if "snapshotmetadataid" in ctx.extra():
            raise error.Abort(
                _(
                    "%s is a snapshot, set ui.allow-checkout-snapshot"
                    " config to True to checkout on it\n"
                )
                % ctx
            )
    return orig(repo, node, overwrite, **opts)


def _updateheads(orig, self, repo, newheads, tr):
    """ensures that we don't try to make the snapshot nodes visible
    """
    unfi = repo
    heads = []
    for h in newheads:
        if h not in unfi:
            continue
        ctx = unfi[h]
        # this way we mostly preserve the correct order
        if "snapshotmetadataid" in ctx.extra():
            heads += [p.node() for p in ctx.parents()]
        else:
            heads.append(h)
    return orig(self, repo, heads, tr)


def _showgraphnode(orig, repo, ctx, **args):
    if "snapshotmetadataid" in ctx.extra():
        return "s"
    return orig(repo, ctx, **args)


def _update(orig, ui, repo, node=None, rev=None, **opts):
    allowsnapshots = repo.ui.configbool("ui", "allow-checkout-snapshot")
    unfi = repo
    if not allowsnapshots and node in unfi:
        ctx = unfi[node]
        if "snapshotmetadataid" in ctx.extra():
            ui.warn(
                _(
                    "%s is a snapshot, set ui.allow-checkout-snapshot"
                    " config to True to checkout on it directly\n"
                    "Executing `hg snapshot checkout %s`.\n"
                )
                % (ctx, ctx)
            )
            return snapshotcommands.snapshotcheckout(ui, repo, node, **opts)
    return orig(ui, repo, node=node, rev=rev, **opts)


def _handlebundle2part(orig, self, bundle, part):
    if part.type != bundleparts.snapshotmetadataparttype:
        return orig(self, bundle, part)
    self._snapshotbundlestore = blobstoremod.memlocal()
    for oid, data in bundleparts.binarydecode(part):
        self._snapshotbundlestore.write(oid, data)


def _smartlogrevset(orig, repo, subset, x):
    revs = orig(repo, subset, x)
    snapshotstring = revsetlang.formatspec("snapshot()")
    return smartset.addset(revs, repo.anyrevs([snapshotstring], user=True))


def _dounhide(orig, repo, revs):
    """prevents the snapshot nodes from being visible
    """
    unfi = repo
    revs = [r for r in revs if "snapshotmetadataid" not in unfi[r].extra()]
    if len(revs) > 0:
        orig(repo, revs)


revsetpredicate = registrar.revsetpredicate()


@revsetpredicate("snapshot")
def snapshot(repo, subset, x):
    """Snapshot changesets"""
    unfi = repo
    # get all the hex nodes of snapshots from the file
    nodes = repo.snapshotlist.snapshots
    return subset & unfi.revs("%ls", nodes)
