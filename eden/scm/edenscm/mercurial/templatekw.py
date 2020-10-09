# Portions Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# templatekw.py - common changeset template keywords
#
# Copyright 2005-2009 Matt Mackall <mpm@selenic.com>
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

from . import (
    encoding,
    error,
    hbisect,
    i18n,
    mutation,
    obsutil,
    patch,
    pycompat,
    registrar,
    scmutil,
    templatenew as templatefixtures,
    util,
)
from .i18n import _
from .node import hex, nullid


class _hybrid(object):
    """Wrapper for list or dict to support legacy template

    This class allows us to handle both:
    - "{files}" (legacy command-line-specific list hack) and
    - "{files % '{file}\n'}" (hgweb-style with inlining and function support)
    and to access raw values:
    - "{ifcontains(file, files, ...)}", "{ifcontains(key, extras, ...)}"
    - "{get(extras, key)}"
    - "{files|json}"
    """

    def __init__(self, gen, values, makemap, joinfmt, keytype=None):
        if gen is not None:
            self.gen = gen  # generator or function returning generator
        self._values = values
        self._makemap = makemap
        self.joinfmt = joinfmt
        self.keytype = keytype  # hint for 'x in y' where type(x) is unresolved

    def gen(self):
        """Default generator to stringify this as {join(self, ' ')}"""
        for i, x in enumerate(self._values):
            if i > 0:
                yield " "
            yield self.joinfmt(x)

    def itermaps(self):
        makemap = self._makemap
        for x in self._values:
            yield makemap(x)

    def __contains__(self, x):
        return x in self._values

    def __getitem__(self, key):
        return self._values[key]

    def __len__(self):
        return len(self._values)

    def __iter__(self):
        return iter(self._values)

    def __getattr__(self, name):
        if name not in (
            "get",
            "items",
            "iteritems",
            "iterkeys",
            "itervalues",
            "keys",
            "values",
        ):
            raise AttributeError(name)
        return getattr(self._values, name)


class _mappable(object):
    """Wrapper for non-list/dict object to support map operation

    This class allows us to handle both:
    - "{manifest}"
    - "{manifest % '{rev}:{node}'}"
    - "{manifest.rev}"

    Unlike a _hybrid, this does not simulate the behavior of the underling
    value. Use unwrapvalue() or unwraphybrid() to obtain the inner object.
    """

    def __init__(self, gen, key, value, makemap):
        if gen is not None:
            self.gen = gen  # generator or function returning generator
        self._key = key
        self._value = value  # may be generator of strings
        self._makemap = makemap

    def gen(self):
        yield pycompat.bytestr(self._value)

    def tomap(self):
        return self._makemap(self._key)

    def itermaps(self):
        yield self.tomap()


def hybriddict(data, key="key", value="value", fmt="%s=%s", gen=None):
    """Wrap data to support both dict-like and string-like operations"""
    return _hybrid(
        gen, data, lambda k: {key: k, value: data[k]}, lambda k: fmt % (k, data[k])
    )


def hybridlist(data, name, fmt="%s", gen=None):
    """Wrap data to support both list-like and string-like operations"""
    return _hybrid(gen, data, lambda x: {name: x}, lambda x: fmt % x)


def unwraphybrid(thing):
    """Return an object which can be stringified possibly by using a legacy
    template"""
    gen = getattr(thing, "gen", None)
    if gen is None:
        return thing
    if callable(gen):
        return gen()
    return gen


def unwrapvalue(thing):
    """Move the inner value object out of the wrapper"""
    if not util.safehasattr(thing, "_value"):
        return thing
    return thing._value


def wraphybridvalue(container, key, value):
    """Wrap an element of hybrid container to be mappable

    The key is passed to the makemap function of the given container, which
    should be an item generated by iter(container).
    """
    makemap = getattr(container, "_makemap", None)
    if makemap is None:
        return value
    if util.safehasattr(value, "_makemap"):
        # a nested hybrid list/dict, which has its own way of map operation
        return value
    return _mappable(None, key, value, makemap)


def showdict(
    name,
    data,
    mapping,
    plural=None,
    key="key",
    value="value",
    fmt="%s=%s",
    separator=" ",
):
    c = [{key: k, value: v} for k, v in pycompat.iteritems(data)]
    f = _showlist(name, c, mapping, plural, separator)
    return hybriddict(data, key=key, value=value, fmt=fmt, gen=f)


def showlist(name, values, mapping, plural=None, element=None, separator=" "):
    if not element:
        element = name
    f = _showlist(name, values, mapping, plural, separator)
    return hybridlist(values, name=element, gen=f)


def _showlist(name, values, mapping, plural=None, separator=" "):
    """expand set of values.
    name is name of key in template map.
    values is list of strings or dicts.
    plural is plural of name, if not simply name + 's'.
    separator is used to join values as a string

    expansion works like this, given name 'foo'.

    if values is empty, expand 'no_foos'.

    if 'foo' not in template map and values are strings, return a string
    containing all values joined by 'separator'. if values are not strings,
    return 'N foos' where N is the length of the list.

    expand 'start_foos'.

    for each value, expand 'foo'. if 'last_foo' in template
    map, expand it instead of 'foo' for last key.

    expand 'end_foos'.
    """
    templ = mapping["templ"]
    strmapping = mapping
    if not plural:
        plural = name + "s"
    if not values:
        noname = "no_" + plural
        if noname in templ:
            yield templ(noname, **strmapping)
        return
    if name not in templ:
        if isinstance(values[0], str):
            yield separator.join(values)
        else:
            count = len(values)
            yield "%s %s" % (count, name if count == 1 else plural)
        return
    startname = "start_" + plural
    if startname in templ:
        yield templ(startname, **strmapping)
    vmapping = mapping.copy()

    def one(v, tag=name):
        try:
            vmapping.update(v)
        except (AttributeError, ValueError):
            try:
                for a, b in v:
                    vmapping[a] = b
            except ValueError:
                vmapping[name] = v
        return templ(tag, **vmapping)

    lastname = "last_" + name
    if lastname in templ:
        last = values.pop()
    else:
        last = None
    for v in values:
        yield one(v)
    if last is not None:
        yield one(last, tag=lastname)
    endname = "end_" + plural
    if endname in templ:
        yield templ(endname, **strmapping)


def getfiles(repo, ctx, revcache):
    if "files" not in revcache:
        revcache["files"] = repo.status(ctx.p1(), ctx)[:3]
    return revcache["files"]


def getrenamedfn(repo, endrev=None):
    rcache = {}
    if endrev is None:
        endrev = len(repo)

    def getrenamed(fn, rev):
        """looks up all renames for a file (up to endrev) the first
        time the file is given. It indexes on the changerev and only
        parses the manifest if linkrev != changerev.
        Returns rename info for fn at changerev rev."""
        if fn not in rcache:
            rcache[fn] = {}
            fl = repo.file(fn)
            for i in fl:
                lr = fl.linkrev(i)
                renamed = fl.renamed(fl.node(i))
                rcache[fn][lr] = renamed
                if lr >= endrev:
                    break
        if rev in rcache[fn]:
            return rcache[fn][rev]

        # If linkrev != rev (i.e. rev not found in rcache) fallback to
        # filectx logic.
        try:
            return repo[rev][fn].renamed()
        except error.LookupError:
            return None

    return getrenamed


def getlogcolumns():
    """Return a dict of log column labels"""
    columns = templatefixtures.logcolumns

    # callsite wants 'changeset' to exist as a dict key.
    def normalize(name):
        if name == "commit":
            return "changeset"
        else:
            return name

    return dict(
        zip(
            [normalize(s.split(":", 1)[0]) for s in columns.splitlines()],
            i18n._(columns).splitlines(True),
        )
    )


# default templates internally used for rendering of lists
defaulttempl = templatefixtures.defaulttempl

# keywords are callables like:
# fn(repo, ctx, templ, cache, revcache, **args)
# with:
# repo - current repository instance
# ctx - the changectx being displayed
# templ - the templater instance
# cache - a cache dictionary for the whole templater run
# revcache - a cache dictionary for the current revision
keywords = {}

templatekeyword = registrar.templatekeyword(keywords)


@templatekeyword("author")
def showauthor(repo, ctx, templ, **args):
    """String. The unmodified author of the changeset."""
    return ctx.user()


@templatekeyword("bisect")
def showbisect(repo, ctx, templ, **args):
    """String. The changeset bisection status."""
    return hbisect.label(repo, ctx.node())


@templatekeyword("branch")
def showbranch(**args):
    """String. The name of the branch on which the changeset was
    committed.
    """
    return args[r"ctx"].branch()


@templatekeyword("branches")
def showbranches(**args):
    """List of strings. The name of the branch on which the
    changeset was committed. Will be empty if the branch name was
    default. (DEPRECATED)
    """
    args = args
    branch = args["ctx"].branch()
    if branch != "default":
        return showlist("branch", [branch], args, plural="branches")
    return showlist("branch", [], args, plural="branches")


@templatekeyword("bookmarks")
def showbookmarks(**args):
    """List of strings. Any bookmarks associated with the
    changeset. Also sets 'active', the name of the active bookmark.
    """
    args = args
    repo = args["ctx"]._repo
    bookmarks = args["ctx"].bookmarks()
    active = repo._activebookmark
    makemap = lambda v: {"bookmark": v, "active": active, "current": active}
    f = _showlist("bookmark", bookmarks, args)
    return _hybrid(f, bookmarks, makemap, pycompat.identity)


@templatekeyword("children")
def showchildren(**args):
    """List of strings. The children of the changeset."""
    args = args
    ctx = args["ctx"]
    if ctx._repo.ui.plain():
        childrevs = ["%d:%s" % (cctx, cctx) for cctx in ctx.children()]
    else:
        childrevs = ["%s" % cctx for cctx in ctx.children()]
    return showlist("children", childrevs, args, element="child")


# Deprecated, but kept alive for help generation a purpose.
@templatekeyword("currentbookmark")
def showcurrentbookmark(**args):
    """String. The active bookmark, if it is associated with the changeset.
    (DEPRECATED)"""
    return showactivebookmark(**args)


@templatekeyword("activebookmark")
def showactivebookmark(**args):
    """String. The active bookmark, if it is associated with the changeset."""
    active = args[r"repo"]._activebookmark
    if active and active in args[r"ctx"].bookmarks():
        return active
    return ""


@templatekeyword("date")
def showdate(repo, ctx, templ, **args):
    """Date information. The date when the changeset was committed."""
    return ctx.date()


@templatekeyword("desc")
def showdescription(repo, ctx, templ, **args):
    """String. The text of the changeset description."""
    s = ctx.description()
    if isinstance(s, encoding.localstr):
        # try hard to preserve utf-8 bytes
        return encoding.tolocal(encoding.fromlocal(s).strip())
    else:
        return s.strip()


@templatekeyword("diffstat")
def showdiffstat(repo, ctx, templ, **args):
    """String. Statistics of changes with the following format:
    "modified files: +added/-removed lines"
    """
    stats = patch.diffstatdata(util.iterlines(ctx.diff(noprefix=False)))
    maxname, maxtotal, adds, removes, binary = patch.diffstatsum(stats)
    return "%s: +%s/-%s" % (len(stats), adds, removes)


@templatekeyword("envvars")
def showenvvars(repo, **args):
    """A dictionary of environment variables. (EXPERIMENTAL)"""
    args = args
    env = repo.ui.exportableenviron()
    env = util.sortdict((k, env[k]) for k in sorted(env))
    return showdict("envvar", env, args, plural="envvars")


@templatekeyword("extras")
def showextras(**args):
    """List of dicts with key, value entries of the 'extras'
    field of this changeset."""
    args = args
    extras = args["ctx"].extra()
    extras = util.sortdict((k, extras[k]) for k in sorted(extras))
    makemap = lambda k: {"key": k, "value": extras[k]}
    c = [makemap(k) for k in extras]
    f = _showlist("extra", c, args, plural="extras")
    return _hybrid(
        f, extras, makemap, lambda k: "%s=%s" % (k, util.escapestr(extras[k]))
    )


@templatekeyword("file_adds")
def showfileadds(**args):
    """List of strings. Files added by this changeset."""
    args = args
    repo, ctx, revcache = args["repo"], args["ctx"], args["revcache"]
    return showlist("file_add", getfiles(repo, ctx, revcache)[1], args, element="file")


@templatekeyword("file_copies")
def showfilecopies(**args):
    """List of strings. Files copied in this changeset with
    their sources.
    """
    args = args
    cache, ctx = args["cache"], args["ctx"]
    copies = args["revcache"].get("copies")
    if copies is None:
        if "getrenamed" not in cache:
            cache["getrenamed"] = getrenamedfn(args["repo"])
        copies = []
        getrenamed = cache["getrenamed"]
        for fn in ctx.files():
            rename = getrenamed(fn, ctx.rev())
            if rename:
                copies.append((fn, rename[0]))

    copies = util.sortdict(copies)
    return showdict(
        "file_copy",
        copies,
        args,
        plural="file_copies",
        key="name",
        value="source",
        fmt="%s (%s)",
    )


# showfilecopiesswitch() displays file copies only if copy records are
# provided before calling the templater, usually with a --copies
# command line switch.
@templatekeyword("file_copies_switch")
def showfilecopiesswitch(**args):
    """List of strings. Like "file_copies" but displayed
    only if the --copied switch is set.
    """
    args = args
    copies = args["revcache"].get("copies") or []
    copies = util.sortdict(copies)
    return showdict(
        "file_copy",
        copies,
        args,
        plural="file_copies",
        key="name",
        value="source",
        fmt="%s (%s)",
    )


@templatekeyword("file_dels")
def showfiledels(**args):
    """List of strings. Files removed by this changeset."""
    args = args
    repo, ctx, revcache = args["repo"], args["ctx"], args["revcache"]
    return showlist("file_del", getfiles(repo, ctx, revcache)[2], args, element="file")


@templatekeyword("file_mods")
def showfilemods(**args):
    """List of strings. Files modified by this changeset."""
    args = args
    repo, ctx, revcache = args["repo"], args["ctx"], args["revcache"]
    return showlist("file_mod", getfiles(repo, ctx, revcache)[0], args, element="file")


@templatekeyword("files")
def showfiles(**args):
    """List of strings. All files modified, added, or removed by this
    changeset.
    """
    args = args
    return showlist("file", list(args["ctx"].files()), args)


@templatekeyword("filestat")
def showfilestat(**args):
    """List of file status objects. Status information for each file affected.

    Each file status object has the following fields:
      - 'op' is one of 'A' for added, 'M' for modified, or 'R' for removed.
      - 'type' is one of 'n' for normal, 'x' for executable, 'l' for link,
        or 'r' for removed.
      - 'size' is the size of the file in bytes.  0 for removed files.
      - 'name' is the name of the file.
    (EXPERIMENTAL)
    """
    repo, ctx, revcache = args[r"repo"], args[r"ctx"], args[r"revcache"]
    files = getfiles(repo, ctx, revcache)
    filestat = []
    for op, filelist in zip("MAR", files):
        for file in filelist:
            stat = {"name": file, "op": op}
            if op == "R":
                stat["type"] = "r"
                stat["size"] = 0
            else:
                filectx = ctx.filectx(file)
                if filectx.islink():
                    stat["type"] = "l"
                elif filectx.isexec():
                    stat["type"] = "x"
                else:
                    stat["type"] = "n"
                stat["size"] = filectx.size()
            filestat.append(stat)
    f = _showlist("filestat", filestat, args)
    return _hybrid(f, filestat, lambda x: x, lambda x: "%s" % x)


@templatekeyword("graphnode")
def showgraphnode(repo, ctx, **args):
    """String. The character representing the changeset node in an ASCII
    revision graph."""
    wpnodes = repo.dirstate.parents()
    if wpnodes[1] == nullid:
        wpnodes = wpnodes[:1]
    if ctx.node() in wpnodes:
        return "@"
    elif ctx.invisible() or ctx.obsolete():
        return "x"
    elif ctx.closesbranch():
        return "_"
    else:
        return "o"


@templatekeyword("graphwidth")
def showgraphwidth(repo, ctx, templ, **args):
    """Integer. The width of the graph drawn by 'log --graph' or zero."""
    # The value args['graphwidth'] will be this function, so we use an internal
    # name to pass the value through props into this function.
    return args.get("_graphwidth", 0)


@templatekeyword("index")
def showindex(**args):
    """Integer. The current iteration of the loop. (0 indexed)"""
    # just hosts documentation; should be overridden by template mapping
    raise error.Abort(_("can't use index in this context"))


@templatekeyword("manifest")
def showmanifest(**args):
    repo, ctx, templ = args[r"repo"], args[r"ctx"], args[r"templ"]
    mnode = ctx.manifestnode()
    if mnode is None:
        # just avoid crash, we might want to use the 'ff...' hash in future
        return
    mrev = repo.manifestlog._revlog.rev(mnode)
    mhex = hex(mnode)
    args = args.copy()
    args.update({r"rev": mrev, r"node": mhex})
    f = templ("manifest", **args)
    # TODO: perhaps 'ctx' should be dropped from mapping because manifest
    # rev and node are completely different from changeset's.
    return _mappable(f, None, f, lambda x: {"rev": mrev, "node": mhex})


@templatekeyword("nodechanges")
def shownodechanges(nodereplacements, **args):
    """List. Rewritten nodes after a command."""
    hexmapping = util.sortdict()
    for oldnode, newnodes in nodereplacements.items():
        hexmapping[hex(oldnode)] = showlist(
            "newnodes", [hex(n) for n in newnodes], args, element="newnode"
        )
    return showdict(
        "nodechange",
        hexmapping,
        args,
        plural="nodechanges",
        key="oldnode",
        value="newnodes",
    )


def shownames(namespace, **args):
    """helper method to generate a template keyword for a namespace"""
    args = args
    ctx = args["ctx"]
    repo = ctx.repo()
    ns = repo.names[namespace]
    names = ns.names(repo, ctx.node())
    return showlist(ns.templatename, names, args, plural=namespace)


@templatekeyword("namespaces")
def shownamespaces(**args):
    """Dict of lists. Names attached to this changeset per
    namespace."""
    args = args
    ctx = args["ctx"]
    repo = ctx.repo()

    namespaces = util.sortdict()

    def makensmapfn(ns):
        # 'name' for iterating over namespaces, templatename for local reference
        return lambda v: {"name": v, ns.templatename: v}

    for k, ns in pycompat.iteritems(repo.names):
        names = ns.names(repo, ctx.node())
        f = _showlist("name", names, args)
        namespaces[k] = _hybrid(f, names, makensmapfn(ns), pycompat.identity)

    f = _showlist("namespace", list(namespaces), args)

    def makemap(ns):
        return {
            "namespace": ns,
            "names": namespaces[ns],
            "builtin": repo.names[ns].builtin,
            "colorname": repo.names[ns].colorname,
        }

    return _hybrid(f, namespaces, makemap, pycompat.identity)


@templatekeyword("node")
def shownode(repo, ctx, templ, **args):
    """String. The changeset identification hash, as a 40 hexadecimal
    digit string.
    """
    return ctx.hex()


@templatekeyword("obsolete")
def showobsolete(repo, ctx, templ, **args):
    """String. Whether the changeset is obsolete. (EXPERIMENTAL)"""
    if ctx.obsolete():
        return "obsolete"
    return ""


@templatekeyword("peerurls")
def showpeerurls(repo, **args):
    """A dictionary of repository locations defined in the [paths] section
    of your configuration file."""
    # see commands.paths() for naming of dictionary keys
    paths = repo.ui.paths
    urls = util.sortdict((k, p.rawloc) for k, p in sorted(pycompat.iteritems(paths)))

    def makemap(k):
        p = paths[k]
        d = {"name": k, "url": p.rawloc}
        d.update((o, v) for o, v in sorted(pycompat.iteritems(p.suboptions)))
        return d

    return _hybrid(None, urls, makemap, lambda k: "%s=%s" % (k, urls[k]))


@templatekeyword("predecessors")
def showpredecessors(repo, ctx, **args):
    """Returns the list if the closest visible predecessors. (EXPERIMENTAL)"""
    if mutation.enabled(repo):
        predecessors = sorted(mutation.predecessorsset(repo, ctx.node(), closest=True))
    else:
        predecessors = sorted(obsutil.closestpredecessors(repo, ctx.node()))
    predecessors = list(map(hex, predecessors))

    return _hybrid(
        None,
        predecessors,
        lambda x: {"ctx": repo[x], "revcache": {}},
        lambda x: scmutil.formatchangeid(repo[x]),
    )


@templatekeyword("successorssets")
def showsuccessorssets(repo, ctx, **args):
    """Returns a string of sets of successors for a changectx. Format used
    is: [ctx1, ctx2], [ctx3] if ctx has been split into ctx1 and ctx2
    while also diverged into ctx3. (EXPERIMENTAL)"""
    if not ctx.obsolete():
        return ""
    args = args

    if mutation.enabled(repo):
        ssets = mutation.successorssets(repo, ctx.node(), closest=True)
    else:
        ssets = obsutil.successorssets(repo, ctx.node(), closest=True)
    ssets = [[hex(n) for n in ss] for ss in ssets]

    data = []
    for ss in ssets:
        h = _hybrid(
            None,
            ss,
            lambda x: {"ctx": repo[x], "revcache": {}},
            lambda x: scmutil.formatchangeid(repo[x]),
        )
        data.append(h)

    # Format the successorssets
    def render(d):
        t = []
        for i in d.gen():
            t.append(i)
        return "".join(t)

    def gen(data):
        yield "; ".join(render(d) for d in data)

    return _hybrid(gen(data), data, lambda x: {"successorset": x}, pycompat.identity)


@templatekeyword("mutations")
def mutations(repo, ctx, **args):
    """Returns a list of the results of mutating the commit.

    Each mutation has the following fields:
      - 'operation' is the name of the mutation operation
      - 'successors' is the list of successor commits for this operation.

    Sequences of mutations that result in a single successor are collapsed into
    a single ``rewrite`` operation.
    """
    descs = []
    if mutation.enabled(repo):
        descs = [
            {
                "operation": op,
                "successors": _hybrid(
                    None,
                    [hex(s) for s in succs],
                    lambda x: {"ctx": repo[x], "revcache": {}},
                    lambda x: scmutil.formatchangeid(repo[x]),
                ),
            }
            for (succs, op) in sorted(mutation.fate(repo, ctx.node()))
        ]
    f = _showlist("mutation", descs, args)
    return _hybrid(f, descs, lambda x: x, lambda x: x["operation"])


@templatekeyword("p1rev")
def showp1rev(repo, ctx, templ, **args):
    """Integer. The repository-local revision number of the changeset's
    first parent, or -1 if the changeset has no parents."""
    return ctx.p1().rev()


@templatekeyword("p2rev")
def showp2rev(repo, ctx, templ, **args):
    """Integer. The repository-local revision number of the changeset's
    second parent, or -1 if the changeset has no second parent."""
    return ctx.p2().rev()


@templatekeyword("p1node")
def showp1node(repo, ctx, templ, **args):
    """String. The identification hash of the changeset's first parent,
    as a 40 digit hexadecimal string. If the changeset has no parents, all
    digits are 0."""
    return ctx.p1().hex()


@templatekeyword("p2node")
def showp2node(repo, ctx, templ, **args):
    """String. The identification hash of the changeset's second
    parent, as a 40 digit hexadecimal string. If the changeset has no second
    parent, all digits are 0."""
    return ctx.p2().hex()


@templatekeyword("parents")
def showparents(**args):
    """List of strings. The parents of the changeset in "rev:node" format."""
    args = args
    repo = args["repo"]
    ctx = args["ctx"]
    pctxs = ctx.parents()
    prevs = [p.rev() for p in pctxs]
    parents = [
        [("rev", p.rev()), ("node", p.hex()), ("phase", p.phasestr())] for p in pctxs
    ]
    f = _showlist("parent", parents, args)
    return _hybrid(
        f,
        prevs,
        lambda x: {"ctx": repo[x], "revcache": {}},
        lambda x: scmutil.formatchangeid(repo[x]),
        keytype=int,
    )


@templatekeyword("phase")
def showphase(repo, ctx, templ, **args):
    """String. The changeset phase name."""
    return ctx.phasestr()


@templatekeyword("phaseidx")
def showphaseidx(repo, ctx, templ, **args):
    """Integer. The changeset phase index. (ADVANCED)"""
    return ctx.phase()


@templatekeyword("rev")
def showrev(repo, ctx, templ, **args):
    """Integer. The repository-local changeset revision number."""
    return scmutil.intrev(ctx)


def showrevslist(name, revs, **args):
    """helper to generate a list of revisions in which a mapped template will
    be evaluated"""
    args = args
    repo = args["ctx"].repo()
    f = _showlist(name, ["%d" % r for r in revs], args)
    return _hybrid(
        f,
        revs,
        lambda x: {name: x, "ctx": repo[x], "revcache": {}},
        pycompat.identity,
        keytype=int,
    )


@templatekeyword("termwidth")
def showtermwidth(repo, ctx, templ, **args):
    """Integer. The width of the current terminal."""
    return repo.ui.termwidth()


@templatekeyword("username")
def showusername(repo, *args, **kwargs):
    """String. The current user specified by configs (ex. 'ui.username')."""
    return repo.ui.username()


@templatekeyword("verbosity")
def showverbosity(ui, **args):
    """String. The current output verbosity in 'debug', 'quiet', 'verbose',
    or ''."""
    # see cmdutil.changeset_templater for priority of these flags
    if ui.debugflag:
        return "debug"
    elif ui.quiet:
        return "quiet"
    elif ui.verbose:
        return "verbose"
    return ""


@templatekeyword("remotenames")
def remotenameskw(**args):
    """:remotenames: List of strings. List of remote names associated with the
    changeset.
    """
    repo, ctx = args["repo"], args["ctx"]

    remotenames = []
    if "remotebookmarks" in repo.names:
        remotenames = repo.names["remotebookmarks"].names(repo, ctx.node())

    return showlist("remotename", remotenames, args, plural="remotenames")


def loadkeyword(ui, extname, registrarobj):
    """Load template keyword from specified registrarobj
    """
    for name, func in pycompat.iteritems(registrarobj._table):
        keywords[name] = func


# tell hggettext to extract docstrings from these functions:
i18nfunctions = keywords.values()
