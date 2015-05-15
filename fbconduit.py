# fbconduit.py
#
# An extension to query remote servers for extra information via conduit RPC
#
# Copyright 2015 Facebook, Inc.

from mercurial import templater, extensions, revset, templatekw
from mercurial.i18n import _
import re
import json
from urllib import urlencode
import httplib

conduit_host = None
conduit_path = None
connection = None

MAX_CONNECT_RETRIES = 3

class ConduitError(Exception):
    pass

class HttpError(Exception):
    pass

githashre = re.compile('g([0-9a-fA-F]{40,40})')

def extsetup(ui):
    global conduit_host, conduit_path
    conduit_host = ui.config('fbconduit', 'host')
    conduit_path = ui.config('fbconduit', 'path')
    
    if not conduit_host:
        ui.warn('No conduit host specified in config; disabling fbconduit\n')
        return
    templater.funcs['mirrornode'] = mirrornode
    templatekw.keywords['gitnode'] = showgitnode

    revset.symbols['gitnode'] = gitnode
    extensions.wrapfunction(revset, 'stringset', overridestringset)
    revset.symbols['stringset'] = revset.stringset
    revset.methods['string'] = revset.stringset
    revset.methods['symbol'] = revset.stringset

def _call_conduit(method, **kwargs):
    global connection, conduit_host, conduit_path

    # start connection
    if connection is None:
        connection = httplib.HTTPSConnection(conduit_host)

    # send request
    path = conduit_path + method
    args = urlencode({'params': json.dumps(kwargs)})
    for attempt in xrange(MAX_CONNECT_RETRIES):
        try:
            connection.request('POST', path, args, {'Connection': 'Keep-Alive'})
            break;
        except httplib.HTTPException as e:
            connection.connect()
    else:
        raise e

    # read http response
    response = connection.getresponse()
    if response.status != 200:
        raise HttpError(response.reason)
    result = response.read()

    # strip jsonp header and parse
    assert result.startswith('for(;;);')
    result = json.loads(result[8:])

    # check for conduit errors
    if result['error_code']:
        raise ConduitError(result['error_info'])

    # return RPC result
    return result['result']

    # don't close the connection b/c we want to avoid the connection overhead

def mirrornode(ctx, mapping, args):
    '''template: find this commit in other repositories'''

    reponame = mapping['repo'].ui.config('fbconduit', 'reponame')
    if not reponame:
        # We don't know who we are, so we can't ask for a translation
        return ''

    if mapping['ctx'].mutable():
        # Local commits don't have translations
        return ''

    node = mapping['ctx'].hex()
    args = [f(ctx, mapping, a) for f, a in args]
    if len(args) == 1:
        torepo, totype = reponame, args[0]
    else:
        torepo, totype = args

    try:
        result = _call_conduit('scmquery.get.mirrored.revs',
            from_repo=reponame,
            from_scm='hg',
            to_repo=torepo,
            to_scm=totype,
            revs=[node]
        )
    except ConduitError as e:
        if 'unknown revision' not in str(e.args):
            mapping['repo'].ui.warn(str(e.args) + '\n')
        return ''
    return result.get(node, '')

def showgitnode(repo, ctx, templ, **args):
    """Return the git revision corresponding to a given hg rev"""
    reponame = repo.ui.config('fbconduit', 'reponame')
    if not reponame:
        # We don't know who we are, so we can't ask for a translation
        return ''

    if ctx.mutable():
        # Local commits don't have translations
        return ''
    
    try:
        result = _call_conduit('scmquery.get.mirrored.revs',
            from_repo=reponame,
            from_scm='hg',
            to_repo=reponame,
            to_scm='git',
            revs=[ctx.hex()]
        )
    except ConduitError:
        # templates are expected to return an empty string when no data exists
        return ''
    return result[ctx.hex()]

def gitnode(repo, subset, x):
    """``gitnode(id)``
    Return the hg revision corresponding to a given git rev."""
    l = revset.getargs(x, 1, 1, _("id requires one argument"))
    n = revset.getstring(l[0], _("id requires a string"))

    reponame = repo.ui.config('fbconduit', 'reponame')
    if not reponame:
        # We don't know who we are, so we can't ask for a translation
        return subset.filter(lambda r: false)

    peerpath = repo.ui.expandpath('default')
    try:
        result = _call_conduit('scmquery.get.mirrored.revs',
            from_repo=reponame,
            from_scm='git',
            to_repo=reponame,
            to_scm='hg',
            revs=[n]
        )
    except ConduitError as e:
        if 'unknown revision' not in str(e.args):
            mapping['repo'].ui.warn(str(e.args) + '\n')
        return subset.filter(lambda r: false)
    rn = repo[result[n]].rev()
    return subset.filter(lambda r: r == rn)

def overridestringset(orig, repo, subset, x):
    m = githashre.match(x)
    if m is not None:
        return gitnode(repo, subset, ('string', m.group(1)))
    return orig(repo, subset, x)
