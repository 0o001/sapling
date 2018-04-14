# Copyright 2018 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

# Standard Library
import sys
import traceback

# Mercurial
from mercurial.i18n import _
from mercurial import error

def highlightmsg(ui, msg):
    """
    The tag is used to highlight important messages from Commit Cloud
    """
    return "%s %s" % (ui.label('#commitcloud', 'commitcloud.tag'), msg)

def getownerteam(ui):
    return ui.label(
        ui.config(
            'commitcloud',
            'owner_team',
            'the Source Control Team'),
        'commitcloud.team')

'''
Commit Cloud error wrappers
'''

class UnexpectedError(error.Abort):
    def __init__(self, ui, message, *args):
        tb = sys.exc_info()[-1]
        funtionname = traceback.extract_tb(tb, 1)[0][3]

        topic = highlightmsg(ui, _('unexpected error'))
        details = _('the failing function was %s') % funtionname
        contact = _('please contact %s to report the error') % getownerteam(ui)
        message = "%s: %s\n%s\n%s" % (topic, message, details, contact)
        super(UnexpectedError, self).__init__(message, *args)

class RegistrationError(error.Abort):
    def __init__(self, ui, message, *args):
        authenticationhelp = ui.config('commitcloud', 'auth_help')
        if authenticationhelp:
            topic = highlightmsg(ui, _('registration error'))
            details = _(
                'authentication instructions:\n%s') % authenticationhelp.strip()
            contact = _(
                'please contact %s for more information') % getownerteam(ui)
            message = "%s: %s\n%s\n%s" % (topic, message, details, contact)
        super(RegistrationError, self).__init__(message, *args)

class WorkspaceError(error.Abort):
    def __init__(self, ui, message, *args):
        topic = highlightmsg(ui, _('workspace error'))
        details = _('your repo is not connected to any workspace\n'
                    'please run `hg cloudjoin --help` for more details')
        message = "%s: %s\n%s" % (topic, message, details)
        super(WorkspaceError, self).__init__(message, *args)

class ConfigurationError(error.Abort):
    def __init__(self, ui, message, *args):
        topic = highlightmsg(ui, _('unexpected configuration error'))
        contact = _(
            'please contact %s to report misconfiguration') % getownerteam(ui)
        message = "%s: %s\n%s" % (topic, message, contact)
        super(ConfigurationError, self).__init__(message, *args)

class ServiceError(error.Abort):
    '''Commit Cloud errors from remote service'''

    def __init__(self, ui, message, *args):
        topic = highlightmsg(ui, _('error from remote service'))
        details = _('please retry later')
        contact = _(
            'please let %s know if this error persists') % getownerteam(ui)
        message = "%s: '%s'\n%s\n%s" % (topic, message, details, contact)
        super(ServiceError, self).__init__(message, *args)

class InvalidWorkspaceDataError(error.Abort):
    def __init__(self, ui, message, *args):
        topic = highlightmsg(ui, _('invalid workspace data'))
        details = _('please run `hg cloudrecover`')
        message = "%s: '%s'\n%s" % (topic, message, details)
        super(InvalidWorkspaceDataError, self).__init__(message, *args)

'''
Commit Cloud message wrappers
'''

def highlightstatus(ui, msg):
    ui.status(highlightmsg(ui, msg))

def highlightdebug(ui, msg):
    ui.debug(highlightmsg(ui, msg))
