# Copyright 2017 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import
import hashlib
import os
import re
import socket

from .bundleparts import (
    getscratchbookmarkspart,
    getscratchbranchpart,
)
from mercurial import (
    bundle2,
    changegroup,
    cmdutil,
    commands,
    discovery,
    encoding,
    error,
    hg,
    util,
)

from collections import namedtuple
from hgext3rd.extutil import runshellcommand
from mercurial.extensions import wrapfunction, unwrapfunction
from mercurial.node import bin, hex, nullrev
from mercurial.i18n import _

cmdtable = {}
command = cmdutil.command(cmdtable)

backupbookmarktuple = namedtuple('backupbookmarktuple',
                                 ['hostname', 'reporoot', 'localbookmark'])

@command('pushbackup',
         [('', 'background', None, 'run backup in background')])
def backup(ui, repo, dest=None, **opts):
    """
    Pushes commits, bookmarks and heads to infinitepush.
    New non-extinct commits are saved since the last `hg pushbackup`
    or since 0 revision if this backup is the first.
    Local bookmarks are saved remotely as:
        infinitepush/backups/USERNAME/HOST/REPOROOT/bookmarks/LOCAL_BOOKMARK
    Local heads are saved remotely as:
        infinitepush/backups/USERNAME/HOST/REPOROOT/heads/HEAD_HASH
    """

    if opts.get('background'):
        background_cmd = ['hg', 'pushbackup']
        if dest:
            background_cmd.append(dest)
        logfile = ui.config('infinitepush', 'pushbackuplog')
        if logfile:
            background_cmd.extend(('>>', logfile, '2>&1'))
        runshellcommand(' '.join(background_cmd), os.environ)
        return 0

    username = ui.shortuser(ui.username())
    backuptip, bookmarkshash = _readbackupstatefile(username, repo)
    bookmarkstobackup = _getbookmarkstobackup(username, repo)

    # To avoid race conditions save current tip of the repo and backup
    # everything up to this revision.
    currenttiprev = len(repo) - 1
    other = _getremote(repo, ui, dest, **opts)
    outgoing = _getrevstobackup(repo, other, backuptip,
                                currenttiprev, bookmarkstobackup)
    currentbookmarkshash = _getbookmarkshash(bookmarkstobackup)

    # Wrap deltaparent function to make sure that bundle takes less space
    # See _deltaparent comments for details
    wrapfunction(changegroup.cg2packer, 'deltaparent', _deltaparent)
    try:
        bundler = _createbundler(ui, repo, other)
        backup = False
        if outgoing and outgoing.missing:
            backup = True
            bundler.addpart(getscratchbranchpart(repo, other, outgoing,
                                                 confignonforwardmove=False,
                                                 ui=ui, bookmark=None,
                                                 create=False))

        if currentbookmarkshash != bookmarkshash:
            backup = True
            bundler.addpart(getscratchbookmarkspart(other, bookmarkstobackup))

        if backup:
            _sendbundle(bundler, other)
            _writebackupstatefile(repo.svfs, currenttiprev,
                                   currentbookmarkshash)
        else:
            ui.status(_('nothing to backup\n'))
    finally:
        unwrapfunction(changegroup.cg2packer, 'deltaparent', _deltaparent)
    return 0

@command('pullbackup', [
         ('', 'reporoot', '', 'root of the repo to restore'),
         ('', 'user', '', 'user who ran the backup'),
         ('', 'hostname', '', 'hostname of the repo to restore')])
def restore(ui, repo, dest=None, **opts):
    """
    Pulls commits from infinitepush that were previously saved with
    `hg pushbackup`.
    If user has only one backup for the `dest` repo then it will be restored.
    But user may have backed up many local repos that points to `dest` repo.
    These local repos may reside on different hosts or in different
    repo roots. It makes restore ambiguous; `--reporoot` and `--hostname`
    options are used to disambiguate.
    """

    other = _getremote(repo, ui, dest, **opts)

    sourcereporoot = opts.get('reporoot')
    sourcehostname = opts.get('hostname')
    username = opts.get('user') or ui.shortuser(ui.username())

    pattern = _getcommonuserprefix(username) + '/*'
    fetchedbookmarks = other.listkeyspatterns('bookmarks', patterns=[pattern])
    reporoots = set()
    hostnames = set()
    nodestopull = set()
    localbookmarks = {}
    for book, node in fetchedbookmarks.iteritems():
        parsed = _parsebackupbookmark(username, book)
        if parsed:
            if sourcereporoot and sourcereporoot != parsed.reporoot:
                continue
            if sourcehostname and sourcehostname != parsed.hostname:
                continue
            nodestopull.add(node)
            if parsed.localbookmark:
                localbookmarks[parsed.localbookmark] = node
            reporoots.add(parsed.reporoot)
            hostnames.add(parsed.hostname)
        else:
            ui.warn(_('wrong format of backup bookmark: %s') % book)

    if len(hostnames) > 1:
        raise error.Abort(
            _('ambiguous hostname to restore: %s') % sorted(hostnames),
            hint=_('set --hostname to disambiguate'))

    if len(reporoots) > 1:
        raise error.Abort(
            _('ambiguous repo root to restore: %s') % sorted(reporoots),
            hint=_('set --reporoot to disambiguate'))

    pullcmd, pullopts = _getcommandandoptions('^pull')
    pullopts['rev'] = list(nodestopull)
    if dest:
        pullopts['source'] = dest
    result = pullcmd(ui, repo, **pullopts)

    with repo.wlock():
        with repo.lock():
            with repo.transaction('bookmark') as tr:
                for scratchbook, hexnode in localbookmarks.iteritems():
                    repo._bookmarks[scratchbook] = bin(hexnode)
                repo._bookmarks.recordchange(tr)

    return result

_backupedstatefile = 'infinitepushlastbackupedstate'

# Common helper functions

def _getcommonuserprefix(username):
    return '/'.join(('infinitepush', 'backups', username))

def _getcommonprefix(username, repo):
    hostname = socket.gethostname()

    result = '/'.join((_getcommonuserprefix(username), hostname))
    if not repo.origroot.startswith('/'):
        result += '/'
    result += repo.origroot
    if result.endswith('/'):
        result = result[:-1]
    return result

def _getbackupbookmarkprefix(username, repo):
    return '/'.join((_getcommonprefix(username, repo), 'bookmarks'))

def _escapebookmark(bookmark):
    '''
    If `bookmark` contains "bookmarks" as a substring then replace it with
    "bookmarksbookmarks". This will make parsing remote bookmark name
    unambigious.
    '''

    bookmark = encoding.fromlocal(bookmark)
    return bookmark.replace('bookmarks', 'bookmarksbookmarks')

def _unescapebookmark(bookmark):
    bookmark = encoding.tolocal(bookmark)
    return bookmark.replace('bookmarksbookmarks', 'bookmarks')

def _getbackupbookmarkname(username, bookmark, repo):
    bookmark = _escapebookmark(bookmark)
    return '/'.join((_getbackupbookmarkprefix(username, repo), bookmark))

def _getbackupheadprefix(username, repo):
    return '/'.join((_getcommonprefix(username, repo), 'heads'))

def _getbackupheadname(username, hexhead, repo):
    return '/'.join((_getbackupheadprefix(username, repo), hexhead))

def _getremote(repo, ui, dest, **opts):
    path = ui.paths.getpath(dest, default=('default-push', 'default'))
    if not path:
        raise error.Abort(_('default repository not configured!'),
                         hint=_("see 'hg help config.paths'"))
    dest = path.pushloc or path.loc
    return hg.peer(repo, opts, dest)

def _getcommandandoptions(command):
    cmd = commands.table[command][0]
    opts = dict(opt[1:3] for opt in commands.table[command][1])
    return cmd, opts

# Backup helper functions

def _deltaparent(orig, self, revlog, rev, p1, p2, prev):
    # This version of deltaparent prefers p1 over prev to use less space
    dp = revlog.deltaparent(rev)
    if dp == nullrev and not revlog.storedeltachains:
        # send full snapshot only if revlog configured to do so
        return nullrev
    return p1

def _getdefaultbookmarkstobackup(username, repo):
    bookmarkstobackup = {}
    bookmarkstobackup[_getbackupheadprefix(username, repo) + '/*'] = ''
    bookmarkstobackup[_getbackupbookmarkprefix(username, repo) + '/*'] = ''
    return bookmarkstobackup

def _getbookmarkstobackup(username, repo):
    bookmarkstobackup = _getdefaultbookmarkstobackup(username, repo)
    secret = set(ctx.hex() for ctx in repo.set('secret()'))
    for bookmark, node in repo._bookmarks.iteritems():
        bookmark = _getbackupbookmarkname(username, bookmark, repo)
        hexnode = hex(node)
        if hexnode in secret:
            continue
        bookmarkstobackup[bookmark] = hexnode

    for headrev in repo.revs('head() & draft()'):
        hexhead = repo[headrev].hex()
        headbookmarksname = _getbackupheadname(username, hexhead, repo)
        bookmarkstobackup[headbookmarksname] = hexhead

    return bookmarkstobackup

def _getbookmarkshash(bookmarkstobackup):
    currentbookmarkshash = hashlib.sha1()
    for book, node in sorted(bookmarkstobackup.iteritems()):
        currentbookmarkshash.update(book)
        currentbookmarkshash.update(node)
    return currentbookmarkshash.hexdigest()

def _createbundler(ui, repo, other):
    bundler = bundle2.bundle20(ui, bundle2.bundle2caps(other))
    # Disallow pushback because we want to avoid taking repo locks.
    # And we don't need pushback anyway
    capsblob = bundle2.encodecaps(bundle2.getrepocaps(repo,
                                                      allowpushback=False))
    bundler.newpart('replycaps', data=capsblob)
    return bundler

def _sendbundle(bundler, other):
    stream = util.chunkbuffer(bundler.getchunks())
    try:
        other.unbundle(stream, ['force'], other.url())
    except error.BundleValueError as exc:
        raise error.Abort(_('missing support for %s') % exc)

def findcommonoutgoing(repo, other, heads):
    if heads:
        nodes = map(repo.changelog.node, heads)
        return discovery.findcommonoutgoing(repo, other, onlyheads=nodes)
    else:
        return None

def _getrevstobackup(repo, other, backuptip, currenttiprev, bookmarkstobackup):
    # Use unfiltered repo because backuptip may now point to filtered commit
    repo = repo.unfiltered()
    revs = []
    if backuptip <= currenttiprev:
        revset = 'head() & draft() & %d:' % backuptip
        revs = list(repo.revs(revset))

    outgoing = findcommonoutgoing(repo, other, revs)
    rootstofilter = []
    if outgoing:
        # In rare cases it's possible to have node without filelogs only
        # locally. It is possible if remotefilelog is enabled and if node was
        # stripped server-side. In this case we want to filter this
        # nodes and all ancestors out
        for node in outgoing.missing:
            changectx = repo[node]
            for file in changectx.files():
                try:
                    changectx.filectx(file)
                except error.ManifestLookupError:
                    rootstofilter.append(changectx.rev())

    if rootstofilter:
        revstofilter = list(repo.revs('%ld::', rootstofilter))
        revs = set(revs) - set(revstofilter)
        outgoing = findcommonoutgoing(repo, other, revs)
        filteredhexnodes = set([repo[filteredrev].hex()
                                for filteredrev in revstofilter])
        # Use list(...) to make it work in python2 and python3
        for book, hexnode in list(bookmarkstobackup.items()):
            if hexnode in filteredhexnodes:
                del bookmarkstobackup[book]

    return outgoing

def _readbackupstatefile(username, repo):
    backuptipbookmarkshash = repo.svfs.tryread(_backupedstatefile).split(' ')
    backuptip = 0
    # hash of the default bookmarks to backup. This is to prevent backuping of
    # empty repo
    bookmarkshash = _getbookmarkshash(
        _getdefaultbookmarkstobackup(username, repo))
    if len(backuptipbookmarkshash) == 2:
        try:
            backuptip = int(backuptipbookmarkshash[0]) + 1
        except ValueError:
            pass
        if len(backuptipbookmarkshash[1]) == 40:
            bookmarkshash = backuptipbookmarkshash[1]
    return backuptip, bookmarkshash

def _writebackupstatefile(vfs, backuptip, bookmarkshash):
    with vfs(_backupedstatefile, mode="w", atomictemp=True) as f:
        f.write(str(backuptip) + ' ' + bookmarkshash)

# Restore helper functions
def _parsebackupbookmark(username, backupbookmark):
    '''Parses backup bookmark and returns info about it

    Backup bookmark may represent either a local bookmark or a head.
    Returns None if backup bookmark has wrong format or tuple.
    First entry is a hostname where this bookmark came from.
    Second entry is a root of the repo where this bookmark came from.
    Third entry in a tuple is local bookmark if backup bookmark
    represents a local bookmark and None otherwise.
    '''

    backupbookmarkprefix = _getcommonuserprefix(username)
    commonre = '^{0}/([-\w.]+)(/.*)'.format(re.escape(backupbookmarkprefix))
    bookmarkre = commonre + '/bookmarks/(.*)$'
    headsre = commonre + '/heads/[a-f0-9]{40}$'

    match = re.search(bookmarkre, backupbookmark)
    if not match:
        match = re.search(headsre, backupbookmark)
        if not match:
            return None
        # It's a local head not a local bookmark.
        # That's why localbookmark is None
        return backupbookmarktuple(hostname=match.group(1),
                                   reporoot=match.group(2),
                                   localbookmark=None)

    return backupbookmarktuple(hostname=match.group(1),
                               reporoot=match.group(2),
                               localbookmark=_unescapebookmark(match.group(3)))
