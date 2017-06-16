# allowunstable.py - allow certain commands to create unstable changesets
#
# Copyright 2016 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.
"""enables the creation of unstable changesets for supported commands

   Wraps several commands to temporarily add the `allowunstable` value to the
   `experimental.evolution` configuration option for the duration of command
   execution. This lets those commands create unstable changesets and
   thereby allows them be used on changesets in the middle of a stack.

   This extension is intended as a stopgap measure. Ideally, we would just
   enable allowunstable globally, but it is unclear if doing so would break
   any other extensions or functionality. Until that is determined, this
   extension allows allowunstable to be selectively rolled out while keeping
   all of the wrapping required to do so in one place.
"""

from mercurial import extensions
from mercurial import obsolete

def extsetup(ui):
    # Allow the creation of unstable changesets during histedit.
    try:
        histedit = extensions.find('histedit')
    except KeyError:
        pass
    else:
        extensions.wrapfunction(histedit, '_histedit', allowunstable)
        extensions.wrapfunction(histedit, '_histedit',
                                setcreatemarkersop('histedit'))

    # Allow the creation of unstable changesets during split/fold.
    try:
        evolve = extensions.find('evolve')
    except KeyError:
        pass
    else:
        if '^split' in evolve.cmdtable:
            extensions.wrapcommand(evolve.cmdtable, 'split', allowunstable)
            extensions.wrapcommand(evolve.cmdtable, 'split',
                                   setcreatemarkersop('split'))
        if '^fold|squash' in evolve.cmdtable:
            extensions.wrapcommand(evolve.cmdtable, 'fold', allowunstable)
            extensions.wrapcommand(evolve.cmdtable, 'fold',
                                   setcreatemarkersop('fold'))

    # Allow the creation of unstable changesets during record.
    try:
        record = extensions.find('record')
    except KeyError:
        pass
    else:
        extensions.wrapcommand(record.cmdtable, 'record', allowunstable)
        extensions.wrapcommand(record.cmdtable, 'record',
                               setcreatemarkersop('amend'))

    # Allow the creation of unstable changesets during rebase.
    # (Required to use -r without --keep in the middle of a stack.)
    # No need to set operation name since tweakdefaults does this already.
    try:
        rebase = extensions.find('rebase')
    except KeyError:
        pass
    else:
        extensions.wrapcommand(rebase.cmdtable, 'rebase', allowunstable)

        # In addition to wrapping the user-facing command, we also need to
        # wrap the rebase.rebase() function itself so that extensions that
        # call it directly will work correctly.
        extensions.wrapfunction(rebase, 'rebase', allowunstable)

def allowunstable(orig, ui, repo, *args, **kwargs):
    """Wrap a function with the signature orig(ui, repo, *args, **kwargs)
       to temporarily allow the creation of unstable changesets for the
       duration of a call to the fuction.
    """
    config = set(repo.ui.configlist('experimental', 'evolution'))

    # Do nothing if the creation of obsmarkers is disabled.
    if obsolete.createmarkersopt not in config:
        return orig(ui, repo, *args, **kwargs)

    config.add(obsolete.allowunstableopt)
    overrides = {('experimental', 'evolution'): config}
    with repo.ui.configoverride(overrides, 'allowunstable'):
        return orig(ui, repo, *args, **kwargs)

def setcreatemarkersop(operation):
    """Return a wrapper function that sets the 'operation' field in the
       metadata of obsmarkers created by the wrapped function to the given
       operation name. Relies on the tweakdefaults extension to wrap
       obsolete.createmarkers() to use a global config option to
       get the operation name.
    """
    try:
        tweakdefaults = extensions.find('tweakdefaults')
    except KeyError:
        # Return a no-op wrapper if there's no tweakdefaults extension.
        return lambda orig, *args, **kwargs: orig(*args, **kwargs)

    def wrapper(orig, ui, repo, *args, **kwargs):
        overrides = {(tweakdefaults.globaldata,
                      tweakdefaults.createmarkersoperation): operation}
        with repo.ui.configoverride(overrides, 'allowunstable'):
            return orig(ui, repo, *args, **kwargs)

    return wrapper
