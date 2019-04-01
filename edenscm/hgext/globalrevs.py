# globalrevs.py
#
# Copyright 2018 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.
#
# no-check-code

"""extension for providing strictly increasing revision numbers

With this extension enabled, Mercurial starts adding a strictly increasing
revision number to each commit which is accessible through the 'globalrev'
template.

::

    [format]
    # support strictly increasing revision numbers for new repositories.
    useglobalrevs = True

    [globalrevs]
    # Allow new commits through only pushrebase.
    onlypushrebase = True

    # In this configuration, `globalrevs` extension can only be used to query
    # strictly increasing global revision numbers already embedded in the
    # commits. In particular, `globalrevs` won't embed any data in the commits.
    readonly = True

    # Repository name to be used as key for storing global revisions data in the
    # database. If not specified, name specified through the configuration
    # `hqsql.reponame` will be used.
    reponame = customname

    # The starting global revision for a repository. We will only consider the
    # global revisions greater than equal to this value as valid global revision
    # numbers. Note that this implies there maybe commits with global revision
    # number less than this value but there is no guarantee associated those
    # numbers. Therefore, relying on global revision numbers below this value is
    # undefined behaviour.
    startrev = 0

    # If this configuration is true, the `globalrev` and `svnrev` based revsets
    # would be interoperable. In particular, the commands
    #
    #   hg log -r "svnrev(<svnrev>/<globalrev>)"
    #   hg log -r "globalrev(<svnrev>/<globalrev>)"
    #   hg log -r "r<svnrev>/r<globalrev>"
    #   hg log -r "m<svnrev>/m<globalrev>"
    #
    # would resolve to a commit with <svnrev> as the corresponding svn revision
    # number and/or <globalrev> as the corresponding strictly increasing global
    # revision number.
    svnrevinteroperation = False

    # If this configuration is true, we use a cached mapping from `globalrev ->
    # hash` to enable fast lookup of commits based on the globalrev. This
    # mapping can be built using the `updateglobalrevmeta` command.
    fastlookup = False
"""
from __future__ import absolute_import

from edenscm.mercurial import (
    error,
    extensions,
    localrepo,
    namespaces,
    phases,
    registrar,
    revset,
    smartset,
)
from edenscm.mercurial.i18n import _
from edenscm.mercurial.node import bin, hex, nullid
from edenscm.mercurial.rust.bindings import nodemap as nodemapmod

from .hgsql import CorruptionException, executewithsql, ishgsqlbypassed, issqlrepo
from .hgsubversion import svnrevkw, util as svnutil
from .pushrebase import isnonpushrebaseblocked


configtable = {}
configitem = registrar.configitem(configtable)
configitem("format", "useglobalrevs", default=False)
configitem("globalrevs", "fastlookup", default=False)
configitem("globalrevs", "onlypushrebase", default=True)
configitem("globalrevs", "readonly", default=False)
configitem("globalrevs", "reponame", default=None)
configitem("globalrevs", "startrev", default=0)
configitem("globalrevs", "svnrevinteroperation", default=False)

cmdtable = {}
command = registrar.command(cmdtable)
namespacepredicate = registrar.namespacepredicate()
revsetpredicate = registrar.revsetpredicate()
templatekeyword = registrar.templatekeyword()

EXTRASGLOBALREVKEY = "global_rev"
EXTRACONVERTKEY = "convert_revision"
MAPFILE = "globalrev-nodemap"
LASTREVFILE = "globalrev-nodemap/last-rev"


@templatekeyword("globalrev")
def globalrevkw(repo, ctx, **kwargs):
    return _globalrevkw(repo, ctx, **kwargs)


def _globalrevkw(repo, ctx, **kwargs):
    grev = ctx.extra().get(EXTRASGLOBALREVKEY)
    # If the revision number associated with the commit is before the supported
    # starting revision, nothing to do.
    if grev is not None and repo.ui.configint("globalrevs", "startrev") <= int(grev):
        return grev


def _globalrevkwwrapper(orig, repo, ctx, **kwargs):
    return orig(repo, ctx, **kwargs) or svnrevkw(repo=repo, ctx=ctx, **kwargs)


cls = localrepo.localrepository
for reqs in ["_basesupported", "supportedformats"]:
    getattr(cls, reqs).add("globalrevs")


def _newreporequirementswrapper(orig, repo):
    reqs = orig(repo)
    if repo.ui.configbool("format", "useglobalrevs"):
        reqs.add("globalrevs")
    return reqs


def uisetup(ui):
    extensions.wrapfunction(
        localrepo, "newreporequirements", _newreporequirementswrapper
    )

    def _hgsqlwrapper(loaded):
        if loaded:
            hgsqlmod = extensions.find("hgsql")
            extensions.wrapfunction(hgsqlmod, "wraprepo", _sqllocalrepowrapper)

    def _hgsubversionwrapper(loaded):
        if loaded:
            hgsubversionmod = extensions.find("hgsubversion")
            extensions.wrapfunction(
                hgsubversionmod.util, "lookuprev", _lookupsvnrevwrapper
            )

            globalrevsmod = extensions.find("globalrevs")

            extensions.wrapfunction(globalrevsmod, "_globalrevkw", _globalrevkwwrapper)
            extensions.wrapfunction(
                globalrevsmod, "_lookupglobalrev", _lookupglobalrevwrapper
            )

    if ui.configbool("globalrevs", "svnrevinteroperation"):
        extensions.afterloaded("hgsubversion", _hgsubversionwrapper)

    # We only wrap `hgsql` extension for embedding strictly increasing global
    # revision number in commits if the repository has `hgsql` enabled and it is
    # also configured to write data to the commits. Therefore, do not wrap the
    # extension if that is not the case.
    if not ui.configbool("globalrevs", "readonly") and not ishgsqlbypassed(ui):
        extensions.afterloaded("hgsql", _hgsqlwrapper)


def reposetup(ui, repo):
    # Only need the extra functionality on the servers.
    if issqlrepo(repo):
        _validateextensions(["hgsql", "pushrebase"])
        _validaterepo(repo)


def _validateextensions(extensionlist):
    for extension in extensionlist:
        try:
            extensions.find(extension)
        except Exception:
            raise error.Abort(_("%s extension is not enabled") % extension)


def _validaterepo(repo):
    ui = repo.ui

    allowonlypushrebase = ui.configbool("globalrevs", "onlypushrebase")
    if allowonlypushrebase and not isnonpushrebaseblocked(repo):
        raise error.Abort(_("pushrebase using incorrect configuration"))


def _sqllocalrepowrapper(orig, repo):
    # This ensures that the repo is of type `sqllocalrepo` which is defined in
    # hgsql extension.
    orig(repo)

    # This class will effectively extend the `sqllocalrepo` class.
    class globalrevsrepo(repo.__class__):
        def commitctx(self, ctx, error=False):
            # Assign global revs automatically
            extra = dict(ctx.extra())
            extra[EXTRASGLOBALREVKEY] = self.nextrevisionnumber()
            ctx.extra = lambda: extra
            return super(globalrevsrepo, self).commitctx(ctx, error)

        def revisionnumberfromdb(self):
            # This must be executed while the SQL lock is taken
            if not self.hassqlwritelock():
                raise error.ProgrammingError("acquiring globalrev needs SQL write lock")

            reponame = self._globalrevsreponame
            cursor = self.sqlcursor

            cursor.execute(
                "SELECT value FROM revision_references "
                + "WHERE repo = %s AND "
                + "namespace = 'counter' AND "
                + "name='commit' ",
                (reponame,),
            )

            counterresults = cursor.fetchall()
            if len(counterresults) == 1:
                return int(counterresults[0][0])
            elif len(counterresults) == 0:
                raise error.Abort(
                    CorruptionException(
                        _("no commit counters for %s in database") % reponame
                    )
                )
            else:
                raise error.Abort(
                    CorruptionException(
                        _("multiple commit counters for %s in database") % reponame
                    )
                )

        def nextrevisionnumber(self):
            """ get the next strictly increasing revision number for this
            repository.
            """

            if self._nextrevisionnumber is None:
                self._nextrevisionnumber = self.revisionnumberfromdb()

            nextrev = self._nextrevisionnumber
            self._nextrevisionnumber += 1
            return nextrev

        def transaction(self, *args, **kwargs):
            tr = super(globalrevsrepo, self).transaction(*args, **kwargs)
            if tr.count > 1:
                return tr

            def transactionabort(orig):
                self._nextrevisionnumber = None
                return orig()

            extensions.wrapfunction(tr, "_abort", transactionabort)
            return tr

        def _updaterevisionreferences(self, *args, **kwargs):
            super(globalrevsrepo, self)._updaterevisionreferences(*args, **kwargs)

            newcount = self._nextrevisionnumber

            # Only write to database if the global revision number actually
            # changed.
            if newcount is not None:
                reponame = self._globalrevsreponame
                cursor = self.sqlcursor

                cursor.execute(
                    "UPDATE revision_references "
                    + "SET value=%s "
                    + "WHERE repo=%s AND namespace='counter' AND name='commit'",
                    (newcount, reponame),
                )

    repo._globalrevsreponame = (
        repo.ui.config("globalrevs", "reponame") or repo.sqlreponame
    )
    repo._nextrevisionnumber = None
    repo.__class__ = globalrevsrepo


def _lookupsvnrevwrapper(orig, repo, rev):
    return _lookuprev(orig, _lookupglobalrev, repo, rev)


def _lookupglobalrevwrapper(orig, repo, rev):
    return _lookuprev(svnutil.lookuprev, orig, repo, rev)


def _lookuprev(svnrevlookupfunc, globalrevlookupfunc, repo, rev):
    # If the revision number being looked up is before the supported starting
    # global revision, try if it works as a svn revision number.
    lookupfunc = (
        svnrevlookupfunc
        if (repo.ui.configint("globalrevs", "startrev") > rev)
        else globalrevlookupfunc
    )
    return lookupfunc(repo, rev)


class _globalrevmap(object):
    def __init__(self, repo):
        self.map = nodemapmod.nodemap(repo.sharedvfs.join(MAPFILE))
        self.lastrev = int(repo.sharedvfs.tryread(LASTREVFILE) or "0")

    @staticmethod
    def _globalrevtonode(grev):
        return bin(grev.rjust(40, "0"))

    @staticmethod
    def _nodetoglobalrev(grevnode):
        return str(int(hex(grevnode)))

    def add(self, grev, hgnode):
        self.map.add(self._globalrevtonode(grev), hgnode)

    def gethgnode(self, grev):
        return self.map.lookupbyfirst(self._globalrevtonode(grev))

    def getglobalrev(self, hgnode):
        grevnode = self.map.lookupbysecond(hgnode)
        return self._nodetoglobalrev(grevnode) if grevnode is not None else None

    def save(self, repo):
        self.map.flush()
        repo.sharedvfs.write(LASTREVFILE, "%s" % self.lastrev)


def _lookupglobalrev(repo, grev):
    # If the revision number being looked up is before the supported starting
    # global revision, nothing to do.
    if repo.ui.configint("globalrevs", "startrev") > grev:
        return []

    cl = repo.changelog
    changelogrevision = cl.changelogrevision
    tonode = cl.node

    def matchglobalrev(rev):
        commitglobalrev = changelogrevision(rev).extra.get(EXTRASGLOBALREVKEY)
        return commitglobalrev is not None and int(commitglobalrev) == grev

    if repo.ui.configbool("globalrevs", "fastlookup"):
        globalrevmap = _globalrevmap(repo)
        hgnode = globalrevmap.gethgnode(str(grev))
        if hgnode:
            return [hgnode]

    matchedrevs = []
    for rev in repo.revs("reverse(all())"):
        if matchglobalrev(rev):
            matchedrevs.append(tonode(rev))
            break

    return matchedrevs


def _lookupname(repo, name):
    if name.startswith("m") and name[1:].isdigit():
        return _lookupglobalrev(repo, int(name[1:]))


@namespacepredicate("globalrevs", priority=75)
def _getnamespace(_repo):
    return namespaces.namespace(
        listnames=lambda repo: [], namemap=_lookupname, nodemap=lambda repo, node: []
    )


@revsetpredicate("globalrev(number)", safe=True, weight=10)
def _revsetglobalrev(repo, subset, x):
    """Changesets with given global revision number.
    """
    args = revset.getargs(x, 1, 1, "globalrev takes one argument")
    globalrev = revset.getinteger(
        args[0], "the argument to globalrev() must be a number"
    )

    return subset & smartset.baseset(_lookupglobalrev(repo, globalrev))


@command("^updateglobalrevmeta", [], _("hg updateglobalrevmeta"))
def updateglobalrevmeta(ui, repo, *args, **opts):
    """Reads globalrevs from the latest hg commits and adds them to the
    globalrev-hg mapping."""
    with repo.wlock(), repo.lock():
        unfi = repo.unfiltered()
        clnode = unfi.changelog.node
        clrevision = unfi.changelog.changelogrevision
        globalrevmap = _globalrevmap(unfi)

        lastrev = globalrevmap.lastrev
        repolen = len(unfi)
        for rev in xrange(lastrev, repolen):
            commitdata = clrevision(rev)
            extra = commitdata.extra
            grev = extra.get(EXTRASGLOBALREVKEY)
            if grev:
                hgnode = clnode(rev)
                globalrevmap.add(grev, hgnode)
            else:
                convertrev = extra.get(EXTRACONVERTKEY)
                if convertrev:
                    # ex. svn:uuid/path@1234
                    svnrev = convertrev.rsplit("@", 1)[-1]
                    if svnrev:
                        hgnode = clnode(rev)
                        globalrevmap.add(svnrev, hgnode)

        globalrevmap.lastrev = repolen
        globalrevmap.save(unfi)


@command("^globalrev", [], _("hg globalrev"))
def globalrev(ui, repo, *args, **opts):
    """prints out the next global revision number for a particular repository by
    reading it from the database.
    """

    if not issqlrepo(repo):
        raise error.Abort(_("this repository is not a sql backed repository"))

    def _printnextglobalrev():
        ui.status(_("%s\n") % repo.revisionnumberfromdb())

    executewithsql(repo, _printnextglobalrev, sqllock=True)


@command(
    "^initglobalrev",
    [
        (
            "",
            "i-know-what-i-am-doing",
            None,
            _("only run initglobalrev if you know exactly what you're doing"),
        )
    ],
    _("hg initglobalrev START"),
)
def initglobalrev(ui, repo, start, *args, **opts):
    """ initializes the global revision number for a particular repository by
    writing it to the database.
    """

    if not issqlrepo(repo):
        raise error.Abort(_("this repository is not a sql backed repository"))

    if not opts.get("i_know_what_i_am_doing"):
        raise error.Abort(
            _(
                "You must pass --i-know-what-i-am-doing to run this command. "
                + "Only the Mercurial server admins should ever run this."
            )
        )

    try:
        startrev = int(start)
    except ValueError:
        raise error.Abort(_("start must be an integer."))

    def _initglobalrev():
        cursor = repo.sqlcursor
        reponame = repo._globalrevsreponame

        # Our schemas are setup such that this query will fail if we try to
        # update an existing row which is exactly what we desire here.
        cursor.execute(
            "INSERT INTO "
            + "revision_references(repo, namespace, name, value) "
            + "VALUES(%s, 'counter', 'commit', %s)",
            (reponame, startrev),
        )

        repo.sqlconn.commit()

    executewithsql(repo, _initglobalrev, sqllock=True)
