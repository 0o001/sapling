# Portions Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# setdiscovery.py - improved discovery of common nodeset for mercurial
#
# Copyright 2010 Benoit Boissinot <bboissin@gmail.com>
# and Peter Arrenbrecht <peter@arrenbrecht.ch>
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.
"""
Algorithm works in the following way. You have two repository: local and
remote. They both contains a DAG of changelists.

The goal of the discovery protocol is to find one set of node *common*,
the set of nodes shared by local and remote.

One of the issue with the original protocol was latency, it could
potentially require lots of roundtrips to discover that the local repo was a
subset of remote (which is a very common case, you usually have few changes
compared to upstream, while upstream probably had lots of development).

The new protocol only requires one interface for the remote repo: `known()`,
which given a set of changelists tells you if they are present in the DAG.

The algorithm then works as follow:

 - We will be using three sets, `common`, `missing`, `unknown`. Originally
 all nodes are in `unknown`.
 - Take a sample from `unknown`, call `remote.known(sample)`
   - For each node that remote knows, move it and all its ancestors to `common`
   - For each node that remote doesn't know, move it and all its descendants
   to `missing`
 - Iterate until `unknown` is empty

There are a couple optimizations, first is instead of starting with a random
sample of missing, start by sending all heads, in the case where the local
repo is a subset, you computed the answer in one round trip.

Then you can do something similar to the bisecting strategy used when
finding faulty changesets. Instead of random samples, you can try picking
nodes that will maximize the number of nodes that will be
classified with it (since all ancestors or descendants will be marked as well).
"""

from __future__ import absolute_import

import collections
import random

from . import dagutil, error, progress, util
from .i18n import _
from .node import nullid, nullrev, bin


def _updatesample(dag, nodes, sample, quicksamplesize=0):
    """update an existing sample to match the expected size

    The sample is updated with nodes exponentially distant from each head of the
    <nodes> set. (H~1, H~2, H~4, H~8, etc).

    If a target size is specified, the sampling will stop once this size is
    reached. Otherwise sampling will happen until roots of the <nodes> set are
    reached.

    :dag: a dag object from dagutil
    :nodes:  set of nodes we want to discover (if None, assume the whole dag)
    :sample: a sample to update
    :quicksamplesize: optional target size of the sample"""
    # if nodes is empty we scan the entire graph
    if nodes:
        heads = dag.headsetofconnecteds(nodes)
    else:
        heads = dag.heads()
    dist = {}
    visit = collections.deque(heads)
    seen = set()
    factor = 1
    while visit:
        curr = visit.popleft()
        if curr in seen:
            continue
        d = dist.setdefault(curr, 1)
        if d > factor:
            factor *= 2
        if d == factor:
            sample.add(curr)
            if quicksamplesize and (len(sample) >= quicksamplesize):
                return
        seen.add(curr)
        for p in dag.parents(curr):
            if not nodes or p in nodes:
                dist.setdefault(p, d + 1)
                visit.append(p)


def _takequicksample(dag, nodes, size):
    """takes a quick sample of size <size>

    It is meant for initial sampling and focuses on querying heads and close
    ancestors of heads.

    :dag: a dag object
    :nodes: set of nodes to discover
    :size: the maximum size of the sample"""
    sample = dag.headsetofconnecteds(nodes)
    if size <= len(sample):
        return _limitsample(sample, size)
    _updatesample(dag, None, sample, quicksamplesize=size)
    return sample


def _takefullsample(dag, nodes, size):
    sample = dag.headsetofconnecteds(nodes)
    # update from heads
    _updatesample(dag, nodes, sample)
    # update from roots
    _updatesample(dag.inverse(), nodes, sample)
    assert sample
    sample = _limitsample(sample, size)
    if len(sample) < size:
        more = size - len(sample)
        sample.update(random.sample(list(nodes - sample), more))
    return sample


def _limitsample(sample, desiredlen):
    """return a random subset of sample of at most desiredlen item"""
    if util.istest():
        # Stabilize test across Python 2 / Python 3.
        return set(sorted(sample)[:desiredlen])
    if len(sample) > desiredlen:
        sample = set(random.sample(sample, desiredlen))
    return sample


def findcommonheads(
    ui,
    local,
    remote,
    initialsamplesize=100,
    fullsamplesize=200,
    abortwhenunrelated=True,
    ancestorsof=None,
    explicitremoteheads=None,
):
    """Return a tuple (commonheads, anyincoming, remoteheads) used to
    identify missing nodes from or in remote.

    Read the module-level docstring for important concepts: 'common',
    'missing', and 'unknown'.

    To (greatly) reduce round-trips, setting 'ancestorsof' is necessary.
    - Push: Figure out what to push exactly, and pass 'ancestorsof' as the
      heads of them. If it's 'push -r .', 'ancestorsof' should be just the
      commit hash of '.'.
    - Pull: Figure out what remote names to pull (ex. selectivepull), pass the
      current local commit hashes of those bookmark as 'ancestorsof'.

    Parameters:
    - abortwhenunrelated: aborts if 'common' is empty.
    - ancestorsof: heads (in nodes) to consider. 'unknown' is initially
      '::ancestorsof'.
    - explicitremoteheads: if not None, a list of nodes that are known existed
      on the remote server.

    Return values:
    - 'anyincoming' is a boolean. Its usefulness is questionable.
    - 'localheads % commonheads' (in nodes) defines what is unique in the local
       repo.  'localheads' is not returned, but can be calculated via 'local'.
    - 'remoteheads % commonheads' (in nodes) defines what is unique in the
      remote repo. 'remoteheads' might include commit hashes unknown to the
      local repo.
    """
    return _findcommonheadsnew(
        ui,
        local,
        remote,
        initialsamplesize,
        fullsamplesize,
        abortwhenunrelated,
        ancestorsof,
        explicitremoteheads,
    )


def _findcommonheadsnew(
    ui,
    local,
    remote,
    initialsamplesize=100,
    fullsamplesize=200,
    abortwhenunrelated=True,
    ancestorsof=None,
    explicitremoteheads=None,
):
    """New implementation that does not depend on dagutil.py or ancestor.py,
    for easy Rust migration.

    Read the module-level docstring for important concepts: 'common',
    'missing', and 'unknown'.

    Variable names:
    - 'local' prefix: from local
    - 'remote' prefix: from remote, maybe unknown by local
    - 'sample': from local, to be tested by remote
    - 'common' prefix: known by local, known by remote
    - 'unknown' prefix: known by local, maybe unknown by remote
      (unknown means we don't know if it's known by remote or not yet)
    - 'missing' prefix: known by local, unknown by remote

    This function uses binary commit hashes and avoids revision numbers if
    possible. It's not efficient with the revlog backend (correctness first)
    but the Rust DAG will make it possible to be efficient.
    """
    cl = local.changelog
    dag = cl.dag
    start = util.timer()

    isselectivepull = local.ui.configbool(
        "remotenames", "selectivepull"
    ) and local.ui.configbool("remotenames", "selectivepulldiscovery")

    if ancestorsof is None:
        if isselectivepull:
            # With selectivepull, limit heads for discovery for both local and
            # remote repo - no invisible heads for the local repo.
            localheads = local.heads()
        else:
            localheads = list(dag.headsancestors(dag.all()))
    else:
        localheads = ancestorsof

    # localheads can be empty in special case: after initial streamclone,
    # because both remotenames and visible heads are empty. Ensure 'tip' is
    # part of 'localheads' so we don't pull the entire repo.
    # TODO: Improve clone protocol so streamclone transfers remote names.
    if not localheads:
        localheads = [local["tip"].node()]

    # Filter out 'nullid' immediately.
    localheads = sorted(h for h in localheads if h != nullid)
    unknown = set()
    commonheads = set()

    def sampleunknownboundary(size):
        if not commonheads:
            # Avoid calculating heads(unknown) + roots(unknown) as it can be
            # quite expensive if 'unknown' is large (when there are no common
            # heads).
            # TODO: Revisit this after segmented changelog, which makes it
            # much cheaper.
            return []
        boundary = set(local.nodes("heads(%ln) + roots(%ln)", unknown, unknown))
        picked = _limitsample(boundary, size)
        if boundary:
            ui.debug(
                "sampling from both directions (%d of %d)\n"
                % (len(picked), len(boundary))
            )
        return list(picked)

    def sampleunknownrandom(size):
        size = min(size, len(unknown))
        ui.debug("sampling undecided commits (%d of %d)\n" % (size, len(unknown)))
        return list(_limitsample(unknown, size))

    def samplemultiple(funcs, size):
        """Call multiple sample functions, up to limited size"""
        sample = set()
        for func in funcs:
            picked = func(size - len(sample))
            assert len(picked) <= size
            sample.update(picked)
            if len(sample) >= size:
                break
        return sorted(sample)

    from .bookmarks import selectivepullbookmarknames, remotenameforurl

    sample = set(_limitsample(localheads, initialsamplesize))
    remotename = remotenameforurl(ui, remote.url())  # ex. 'default' or 'remote'
    selected = list(selectivepullbookmarknames(local, remotename))

    # Include names (public heads) that the server might have in sample.
    # This can efficiently extend the "common" set, if the server does
    # have them.
    for name in selected:
        if name in local:
            node = local[name].node()
            if node not in sample:
                sample.add(node)

    # Drop nullid special case.
    sample.discard(nullid)
    sample = sorted(sample)

    ui.debug("query 1; heads\n")
    batch = remote.iterbatch()
    if isselectivepull:
        # With selectivepull, limit heads for discovery for both local and
        # remote repo - only list selected heads on remote.
        # Return type: sorteddict[name: str, hex: str].
        batch.listkeyspatterns("bookmarks", patterns=selected)
    else:
        # Legacy pull: list all heads on remote.
        # Return type: List[node: bytes].
        batch.heads()
    batch.known(sample)
    batch.submit()
    remoteheads, remotehassample = batch.results()

    # If the server has no selected names (ex. master), fallback to fetch all
    # heads.
    #
    # Note: This behavior is not needed for production use-cases. However, many
    # tests setup the server repo without a "master" bookmark. They need the
    # fallback path to not error out like "repository is unrelated" (details
    # in the note below).
    if not remoteheads and isselectivepull:
        isselectivepull = False
        remoteheads = remote.heads()

    # Normalize 'remoteheads' to Set[node].
    if isselectivepull:
        remoteheads = set(bin(h) for h in remoteheads.values())
    else:
        remoteheads = set(remoteheads)

    # Unconditionally include 'explicitremoteheads', if selectivepull is used.
    #
    # Without selectivepull, the "remoteheads" should already contain all the
    # heads and there is no need to consider explicitremoteheads.
    #
    # Note: It's actually a bit more complicated with non-Mononoke infinitepush
    # branches - those heads are not visible via "remote.heads()". There are
    # tests relying on scratch heads _not_ visible in "remote.heads()" to
    # return early (both commonheads and remoteheads are empty) and not error
    # out like "repository is unrelated".
    if explicitremoteheads and isselectivepull:
        remoteheads = remoteheads.union(explicitremoteheads)
    # Remove 'nullid' that the Rust layer dislikes.
    remoteheads = sorted(h for h in remoteheads if h != nullid)

    if cl.tip() == nullid:
        # The local repo is empty. Everything is 'unknown'.
        return [], bool(remoteheads), remoteheads

    ui.status_err(_("searching for changes\n"))

    commonremoteheads = cl.filternodes(remoteheads)

    # Mononoke tests do not want this output.
    ui.debug(
        "local heads: %s; remote heads: %s (explicit: %s); initial common: %s\n"
        % (
            len(localheads),
            len(remoteheads),
            len(explicitremoteheads or ()),
            len(commonremoteheads),
        )
    )

    # fast paths

    if len(commonremoteheads) == len(remoteheads):
        ui.debug("all remote heads known locally\n")
        # TODO: Consider returning [] as the 3rd return value here.
        return remoteheads, False, remoteheads

    commonsample = [n for n, known in zip(sample, remotehassample) if known]
    if set(commonsample).issuperset(set(localheads) - {nullid}):
        ui.note(_("all local heads known remotely\n"))
        # TODO: Check how 'remoteheads' is used at upper layers, and if we
        # can avoid listing all heads remotely (which can be expensive).
        return localheads, True, remoteheads

    # slow path: full blown discovery

    # unknown = localheads % commonheads
    commonheads = dag.sort(commonremoteheads + commonsample)
    unknown = dag.only(localheads, commonheads)
    missing = dag.sort([])

    roundtrips = 1
    with progress.bar(ui, _("searching"), _("queries")) as prog:
        while len(unknown) > 0:
            # Quote from module doc: For each node that remote doesn't know,
            # move it and all its descendants to `missing`.
            missingsample = [
                n for n, known in zip(sample, remotehassample) if not known
            ]
            if missingsample:
                descendants = dag.range(missingsample, localheads)
                missing += descendants
                unknown -= missing

            if not unknown:
                break

            # Decide 'sample'.
            sample = samplemultiple(
                [sampleunknownboundary, sampleunknownrandom], fullsamplesize
            )

            roundtrips += 1
            progmsg = _("checking %i commits, %i left") % (
                len(sample),
                len(unknown) - len(sample),
            )
            prog.value = (roundtrips, progmsg)
            ui.debug(
                "query %i; still undecided: %i, sample size is: %i\n"
                % (roundtrips, len(unknown), len(sample))
            )

            remotehassample = remote.known(sample)

            # Quote from module doc: For each node that remote knows, move it
            # and all its ancestors to `common`.
            # Don't maintain 'common' directly as it's less efficient with
            # revlog backend. Maintain 'commonheads' and 'unknown' instead.
            newcommonheads = [n for n, known in zip(sample, remotehassample) if known]
            if newcommonheads:
                newcommon = dag.only(newcommonheads, commonheads)
                commonheads += dag.sort(newcommonheads)
                unknown -= newcommon

    commonheads = set(dag.headsancestors(commonheads))

    elapsed = util.timer() - start
    ui.debug("%d total queries in %.4fs\n" % (roundtrips, elapsed))
    msg = "found %d common and %d unknown server heads," " %d roundtrips in %.4fs\n"
    remoteonlyheads = set(remoteheads) - commonheads
    ui.log(
        "discovery", msg, len(commonheads), len(remoteonlyheads), roundtrips, elapsed
    )

    if not commonheads and remoteheads:
        if abortwhenunrelated:
            raise error.Abort(_("repository is unrelated"))
        else:
            ui.warn(_("warning: repository is unrelated\n"))
        return [], True, remoteheads

    return sorted(commonheads), True, remoteheads


def _findcommonheadsold(
    ui,
    local,
    remote,
    initialsamplesize=100,
    fullsamplesize=200,
    abortwhenunrelated=True,
    ancestorsof=None,
):
    """The original implementation of findcommonheads using the
    dagutil.revlogdag interface.
    """
    start = util.timer()

    roundtrips = 0
    cl = local.changelog
    localsubset = None
    if ancestorsof is not None:
        rev = local.changelog.rev
        localsubset = [rev(n) for n in ancestorsof]
    dag = dagutil.revlogdag(cl, localsubset=localsubset)

    # early exit if we know all the specified remote heads already
    ui.debug("query 1; heads\n")
    roundtrips += 1
    ownheads = dag.heads()
    sample = _limitsample(ownheads, initialsamplesize)
    # indices between sample and externalized version must match
    sample = list(sample)

    # Always include master in the initial sample since it will convey the most
    # information about the contents of the repo.
    if "master" in local:
        sample.append(local["master"].rev())
    else:
        sample.append(local["tip"].rev())

    batch = remote.iterbatch()
    batch.heads()
    batch.known(dag.externalizeall(sample))
    batch.submit()
    srvheadhashes, yesno = batch.results()

    if cl.tip() == nullid:
        if srvheadhashes != [nullid]:
            return [nullid], True, srvheadhashes
        return [nullid], False, []

    # start actual discovery (we note this before the next "if" for
    # compatibility reasons)
    ui.status_err(_("searching for changes\n"))

    srvheads = dag.internalizeall(srvheadhashes, filterunknown=True)
    if len(srvheads) == len(srvheadhashes):
        ui.debug("all remote heads known locally\n")
        return (srvheadhashes, False, srvheadhashes)

    if sample and len(ownheads) <= initialsamplesize and all(yesno):
        ui.note(_("all local heads known remotely\n"))
        ownheadhashes = dag.externalizeall(ownheads)
        return (ownheadhashes, True, srvheadhashes)

    # full blown discovery

    # own nodes I know we both know
    # treat remote heads (and maybe own heads) as a first implicit sample
    # response
    common = cl.incrementalmissingrevs(srvheads)
    commoninsample = set(n for i, n in enumerate(sample) if yesno[i])
    common.addbases(commoninsample)
    # own nodes where I don't know if remote knows them
    undecided = set(common.missingancestors(ownheads))
    # own nodes I know remote lacks
    missing = set()

    full = False
    with progress.bar(ui, _("searching"), _("queries")) as prog:
        while undecided:

            if sample:
                missinginsample = [n for i, n in enumerate(sample) if not yesno[i]]
                missing.update(dag.descendantset(missinginsample, missing))

                undecided.difference_update(missing)

            if not undecided:
                break

            if full or common.hasbases():
                if full:
                    ui.note(_("sampling from both directions\n"))
                else:
                    ui.debug("taking initial sample\n")
                samplefunc = _takefullsample
                targetsize = fullsamplesize
            else:
                # use even cheaper initial sample
                ui.debug("taking quick initial sample\n")
                samplefunc = _takequicksample
                targetsize = initialsamplesize
            if len(undecided) < targetsize:
                sample = list(undecided)
            else:
                sample = samplefunc(dag, undecided, targetsize)
                sample = _limitsample(sample, targetsize)

            roundtrips += 1
            prog.value = roundtrips
            ui.debug(
                "query %i; still undecided: %i, sample size is: %i\n"
                % (roundtrips, len(undecided), len(sample))
            )
            # indices between sample and externalized version must match
            sample = list(sample)
            yesno = remote.known(dag.externalizeall(sample))
            full = True

            if sample:
                commoninsample = set(n for i, n in enumerate(sample) if yesno[i])
                common.addbases(commoninsample)
                common.removeancestorsfrom(undecided)

    # heads(common) == heads(common.bases) since common represents common.bases
    # and all its ancestors
    result = dag.headsetofconnecteds(common.bases)
    # common.bases can include nullrev, but our contract requires us to not
    # return any heads in that case, so discard that
    result.discard(nullrev)
    elapsed = util.timer() - start
    ui.debug("%d total queries in %.4fs\n" % (roundtrips, elapsed))
    msg = "found %d common and %d unknown server heads," " %d roundtrips in %.4fs\n"
    missing = set(result) - set(srvheads)
    ui.log("discovery", msg, len(result), len(missing), roundtrips, elapsed)

    if not result and srvheadhashes != [nullid]:
        if abortwhenunrelated:
            raise error.Abort(_("repository is unrelated"))
        else:
            ui.warn(_("warning: repository is unrelated\n"))
        return ({nullid}, True, srvheadhashes)

    anyincoming = srvheadhashes != [nullid]
    return dag.externalizeall(result), anyincoming, srvheadhashes
