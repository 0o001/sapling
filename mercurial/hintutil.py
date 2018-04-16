# hint.py - utilities to register hint messages
#
#  Copyright 2018 Facebook Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

from .i18n import _
from . import (
    rcutil,
    util,
)

hinttable = {}
messages = []
triggered = set()

def loadhint(ui, extname, registrarobj):
    for name, func in registrarobj._table.iteritems():
        hinttable[name] = func

def trigger(name, *args, **kwargs):
    """Trigger a hint message. It will be shown at the end of the command."""
    func = hinttable.get(name)
    if func and name not in triggered:
        triggered.add(name)
        msg = func(*args, **kwargs)
        if msg:
            messages.append((name, msg.rstrip()))

def _prefix(ui, name):
    """Return "hint[%s]" % name, colored"""
    return ui.label(_('hint[%s]: ') % (name,), 'hint.prefix')

def show(ui):
    """Show all triggered hint messages"""
    if ui.plain('hint'):
        return
    acked = ui.configlist('hint', 'ack')
    if acked == ['*']:
        def isacked(name):
            return True
    else:
        acked = set(acked)
        def isacked(name):
            return name in acked or ui.configbool('hint', 'ack-%s' % name)
    names = []
    for name, msg in messages:
        if not isacked(name):
            prefix = _prefix(ui, name)
            ui.write_err(('%s%s\n') % (prefix, msg.rstrip()))
            names.append(name)
    if names and not isacked('hint-ack'):
        prefix = _prefix(ui, 'hint-ack')
        msg = (_("use 'hg hint --ack %s' to silence these hints\n")
               % ' '.join(names))
        ui.write_err(prefix + msg)

def silence(ui, names):
    """Silence given hints"""
    path = rcutil.userrcpath()[0]
    acked = ui.configlist('hint', 'ack')
    for name in names:
        if name not in acked:
            acked.append(name)
    value = ' '.join(util.shellquote(w) for w in acked)
    rcutil.editconfig(path, 'hint', 'ack', value)
