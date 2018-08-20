# shallowrepo.py - shallow repository that uses remote filelogs
#
# Copyright 2013 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.
from __future__ import absolute_import

import os

from mercurial import encoding, error, localrepo, match, progress, scmutil, util
from mercurial.i18n import _
from mercurial.node import hex, nullid, nullrev

from . import constants, fileserverclient, remotefilectx, remotefilelog, shallowutil
from ..extutil import runshellcommand
from .contentstore import (
    remotecontentstore,
    remotefilelogcontentstore,
    unioncontentstore,
)
from .datapack import datapackstore
from .historypack import historypackstore
from .metadatastore import (
    remotefilelogmetadatastore,
    remotemetadatastore,
    unionmetadatastore,
)


requirement = "remotefilelog"

# These make*stores functions are global so that other extensions can replace
# them.
def makelocalstores(repo):
    """In-repo stores, like .hg/store/data; can not be discarded."""
    localpath = os.path.join(repo.svfs.vfs.base, "data")
    if not os.path.exists(localpath):
        os.makedirs(localpath)

    # Instantiate local data stores
    localcontent = remotefilelogcontentstore(repo, localpath, repo.name, shared=False)
    localmetadata = remotefilelogmetadatastore(repo, localpath, repo.name, shared=False)

    repo.localdatastores.append(localcontent)
    repo.localhistorystores.append(localmetadata)

    return localcontent, localmetadata


def makecachestores(repo):
    """Typically machine-wide, cache of remote data; can be discarded."""
    # Instantiate shared cache stores
    cachepath = shallowutil.getcachepath(repo.ui)
    cachecontent = remotefilelogcontentstore(repo, cachepath, repo.name, shared=True)
    cachemetadata = remotefilelogmetadatastore(repo, cachepath, repo.name, shared=True)

    repo.sharedstore = cachecontent
    repo.shareddatastores.append(cachecontent)
    repo.sharedhistorystores.append(cachemetadata)

    return cachecontent, cachemetadata


def makeremotestores(repo, cachecontent, cachemetadata):
    """These stores fetch data from a remote server."""
    # Instantiate remote stores
    repo.fileservice = fileserverclient.fileserverclient(repo)
    remotecontent = remotecontentstore(repo.ui, repo.fileservice, cachecontent)
    remotemetadata = remotemetadatastore(repo.ui, repo.fileservice, cachemetadata)
    return remotecontent, remotemetadata


def makepackstores(repo):
    """Packs are more efficient (to read from) cache stores."""

    def makepackstore(datastores, historystores, packpath, deletecorrupt=False):
        packcontentstore = datapackstore(
            repo.ui,
            packpath,
            usecdatapack=repo.ui.configbool("remotefilelog", "fastdatapack"),
            userustdatapack=repo.ui.configbool("format", "userustdatapack"),
            deletecorruptpacks=deletecorrupt,
        )
        packmetadatastore = historypackstore(
            repo.ui,
            packpath,
            userusthistorypack=repo.ui.configbool("format", "userusthistorypack"),
            deletecorruptpacks=deletecorrupt,
        )
        datastores.append(packcontentstore)
        historystores.append(packmetadatastore)

        return packcontentstore, packmetadatastore

    # Instantiate pack stores
    spackpath = shallowutil.getcachepackpath(repo, constants.FILEPACK_CATEGORY)
    spackcontent, spackmetadata = makepackstore(
        repo.shareddatastores, repo.sharedhistorystores, spackpath, deletecorrupt=True
    )

    lpackpath = shallowutil.getlocalpackpath(
        repo.svfs.vfs.base, constants.FILEPACK_CATEGORY
    )
    lpackcontent, lpackmetadata = makepackstore(
        repo.localdatastores, repo.localhistorystores, lpackpath
    )

    shallowutil.reportpackmetrics(
        repo.ui, "filestore", spackcontent, spackmetadata, lpackcontent, lpackmetadata
    )
    return (spackcontent, spackmetadata, lpackcontent, lpackmetadata)


def makeunionstores(repo):
    """Union stores iterate the other stores and return the first result."""
    repo.shareddatastores = []
    repo.sharedhistorystores = []
    repo.localdatastores = []
    repo.localhistorystores = []

    spackcontent, spackmetadata, lpackcontent, lpackmetadata = makepackstores(repo)
    cachecontent, cachemetadata = makecachestores(repo)
    localcontent, localmetadata = makelocalstores(repo)
    remotecontent, remotemetadata = makeremotestores(repo, cachecontent, cachemetadata)

    # Instantiate union stores
    repo.contentstore = unioncontentstore(
        spackcontent,
        cachecontent,
        lpackcontent,
        localcontent,
        remotecontent,
        writestore=localcontent,
    )
    repo.metadatastore = unionmetadatastore(
        spackmetadata,
        cachemetadata,
        lpackmetadata,
        localmetadata,
        remotemetadata,
        writestore=localmetadata,
    )

    fileservicedatawrite = cachecontent
    fileservicehistorywrite = cachemetadata
    if repo.ui.configbool("remotefilelog", "fetchpacks"):
        fileservicedatawrite = spackcontent
        fileservicehistorywrite = spackmetadata
    repo.fileservice.setstore(
        repo.contentstore,
        repo.metadatastore,
        fileservicedatawrite,
        fileservicehistorywrite,
    )
    shallowutil.reportpackmetrics(
        repo.ui, "filestore", spackcontent, spackmetadata, lpackcontent, lpackmetadata
    )


def wraprepo(repo):
    class shallowrepository(repo.__class__):
        @util.propertycache
        def name(self):
            return self.ui.config("remotefilelog", "reponame", "")

        @util.propertycache
        def fallbackpath(self):
            path = self.ui.config(
                "remotefilelog",
                "fallbackpath",
                # fallbackrepo is the old, deprecated name
                self.ui.config(
                    "remotefilelog", "fallbackrepo", self.ui.config("paths", "default")
                ),
            )
            if not path:
                raise error.Abort(
                    "no remotefilelog server " "configured - is your .hg/hgrc trusted?"
                )

            return path

        def maybesparsematch(self, *revs, **kwargs):
            """
            A wrapper that allows the remotefilelog to invoke sparsematch() if
            this is a sparse repository, or returns None if this is not a
            sparse repository.
            """
            if util.safehasattr(self, "sparsematch"):
                return self.sparsematch(*revs, **kwargs)

            return None

        def file(self, f):
            if f[0] == "/":
                f = f[1:]

            if self.shallowmatch(f):
                return remotefilelog.remotefilelog(self.svfs, f, self)
            else:
                return super(shallowrepository, self).file(f)

        def filectx(self, path, changeid=None, fileid=None):
            if self.shallowmatch(path):
                return remotefilectx.remotefilectx(self, path, changeid, fileid)
            else:
                return super(shallowrepository, self).filectx(path, changeid, fileid)

        @localrepo.unfilteredmethod
        def commitctx(self, ctx, error=False):
            """Add a new revision to current repository.
            Revision information is passed via the context argument.
            """

            # some contexts already have manifest nodes, they don't need any
            # prefetching (for example if we're just editing a commit message
            # we can reuse manifest
            if not ctx.manifestnode():
                # prefetch files that will likely be compared
                m1 = ctx.p1().manifest()
                files = []
                for f in ctx.modified() + ctx.added():
                    fparent1 = m1.get(f, nullid)
                    if fparent1 != nullid:
                        files.append((f, hex(fparent1)))
                self.fileservice.prefetch(files)
            return super(shallowrepository, self).commitctx(ctx, error=error)

        def backgroundprefetch(
            self, revs, base=None, repack=False, pats=None, opts=None
        ):
            """Runs prefetch in background with optional repack
            """
            cmd = [util.hgexecutable(), "-R", self.origroot, "prefetch"]
            if repack:
                cmd.append("--repack")
            if revs:
                cmd += ["-r", revs]
            if base:
                cmd += ["-b", base]
            cmd = " ".join(map(util.shellquote, cmd))

            runshellcommand(cmd, encoding.environ)

        def prefetch(self, revs, base=None, pats=None, opts=None, matcher=None):
            """Prefetches all the necessary file revisions for the given revs
            Optionally runs repack in background
            """
            with self._lock(
                self.svfs,
                "prefetchlock",
                True,
                None,
                None,
                _("prefetching in %s") % self.origroot,
            ):
                self._prefetch(revs, base, pats, opts, matcher)

        def _prefetch(self, revs, base=None, pats=None, opts=None, matcher=None):
            fallbackpath = self.fallbackpath
            if fallbackpath:
                # If we know a rev is on the server, we should fetch the server
                # version of those files, since our local file versions might
                # become obsolete if the local commits are stripped.
                with progress.spinner(self.ui, _("finding outgoing revisions")):
                    localrevs = self.revs("outgoing(%s)", fallbackpath)
                if base is not None and base != nullrev:
                    serverbase = list(
                        self.revs("first(reverse(::%s) - %ld)", base, localrevs)
                    )
                    if serverbase:
                        base = serverbase[0]
            else:
                localrevs = self

            mfl = self.manifestlog
            if base is not None:
                mfdict = mfl[self[base].manifestnode()].read()
                skip = set(mfdict.iteritems())
            else:
                skip = set()

            # Copy the skip set to start large and avoid constant resizing,
            # and since it's likely to be very similar to the prefetch set.
            files = skip.copy()
            serverfiles = skip.copy()
            visited = set()
            visited.add(nullid)
            with progress.bar(self.ui, _("prefetching"), total=len(revs)) as prog:
                for rev in sorted(revs):
                    ctx = self[rev]
                    if pats:
                        m = scmutil.match(ctx, pats, opts)
                    if matcher is None:
                        matcher = self.maybesparsematch(rev)

                    mfnode = ctx.manifestnode()
                    mfctx = mfl[mfnode]

                    # Decompressing manifests is expensive.
                    # When possible, only read the deltas.
                    p1, p2 = mfctx.parents
                    if p1 in visited and p2 in visited:
                        mfdict = mfl[mfnode].readfast()
                    else:
                        mfdict = mfl[mfnode].read()

                    diff = mfdict.iteritems()
                    if pats:
                        diff = (pf for pf in diff if m(pf[0]))
                    if matcher:
                        diff = (pf for pf in diff if matcher(pf[0]))
                    if rev not in localrevs:
                        serverfiles.update(diff)
                    else:
                        files.update(diff)

                    visited.add(mfctx.node())
                    prog.value += 1

            files.difference_update(skip)
            serverfiles.difference_update(skip)

            # Fetch files known to be on the server
            if serverfiles:
                results = [(path, hex(fnode)) for (path, fnode) in serverfiles]
                self.fileservice.prefetch(results, force=True)

            # Fetch files that may or may not be on the server
            if files:
                results = [(path, hex(fnode)) for (path, fnode) in files]
                self.fileservice.prefetch(results)

        def invalidate(self, **kwargs):
            super(shallowrepository, self).invalidate(**kwargs)
            makeunionstores(self)

    repo.__class__ = shallowrepository

    repo.shallowmatch = match.always(repo.root, "")

    makeunionstores(repo)

    repo.includepattern = repo.ui.configlist("remotefilelog", "includepattern", None)
    repo.excludepattern = repo.ui.configlist("remotefilelog", "excludepattern", None)

    if repo.includepattern or repo.excludepattern:
        repo.shallowmatch = match.match(
            repo.root, "", None, repo.includepattern, repo.excludepattern
        )
