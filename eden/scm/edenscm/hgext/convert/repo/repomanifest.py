# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# repo_source.py - Google Repo support for the convert extension (EXPERIMENTAL)

from __future__ import absolute_import

import re

# pyre-fixme[21]: Could not find `dom`.
import xml.dom.minidom
from typing import Any, Callable, Dict, List, Optional, Tuple, Union

from edenscm.hgext.convert import common
from edenscm.mercurial.i18n import _


class InvalidManifestException(Exception):
    """Represents an exception caused by a manifest that violates the rules for manifest contents."""

    pass


class repomanifest(object):
    """Represents a specific instance of a repo's manifest. It's important to
    keep this separate from the repo object because one repo's manifest will
    change by version and branch."""

    @classmethod
    def fromgit(cls, ui, gitpath, version, filepath):
        # type: (Any, str, str, str) -> Any
        """Fetches the contents of a repo manifest

        ui: user interface context
        gitpath: the root directory of the manifest git repo
        version: the commit hash or branch name of the manifest to fetch
        filepath: the filename of the manifest to fetch
        returns: a repo manifest object representing that manifest
        """
        # TODO: arg checking including path

        def fetchfromgit(filename):
            # type: (str) -> str
            commandline = common.commandline(ui, "git")
            output, exitcode = commandline.run(
                "-C", gitpath, "show", "%s:%s" % (version, filename)
            )
            commandline.checkexit(
                exitcode, _("Failed to get manifest %s:%s") % (version, filename)
            )
            return output

        return repomanifest(filepath, fetchfn=fetchfromgit)

    @classmethod
    def frompath(cls, path):
        # type: (str) -> Any
        def fetchpath(path):
            # type: (str) -> str
            with open(path, "r") as f:
                text = f.read()
                return text

        return repomanifest(path, fetchfn=fetchpath)

    @classmethod
    def fromtext(cls, rootblobname, fileblobs):
        # type: (str, Dict[str, str]) -> Any
        """Instantiates a manifest from one or more blobs of text. The root is treated as the root manifest and other blobs provided are available as includes.

        rootblobname: the name of blob in the dictionary to use as the root manifest
        fileblobs: dictionary of blobs of manifest data, keyed by name
        """
        # TODO: arg checking
        if rootblobname not in fileblobs:
            raise KeyError(
                _('Could not find blob "%s" in dictionary of blobs') % rootblobname
            )
        fetchblobfn = lambda filename: fileblobs[filename]
        return repomanifest(rootblobname, fetchfn=fetchblobfn)

    DOM_ELEMENT_ANNOTATION = "annotation"
    DOM_ELEMENT_DEFAULT = "default"
    DOM_ELEMENT_INCLUDE = "include"
    DOM_ELEMENT_LINKFILE = "linkfile"
    DOM_ELEMENT_MANIFEST = "manifest"
    DOM_ELEMENT_PROJECT = "project"
    DOM_ELEMENT_REMOTE = "remote"

    DOM_ATTRIBUTE_DEST = "dest"
    DOM_ATTRIBUTE_FETCH = "fetch"
    DOM_ATTRIBUTE_GROUPS = "groups"
    DOM_ATTRIBUTE_NAME = "name"
    DOM_ATTRIBUTE_PATH = "path"
    DOM_ATTRIBUTE_REMOTE = "remote"
    DOM_ATTRIBUTE_REVISION = "revision"
    DOM_ATTRIBUTE_SRC = "src"
    DOM_ATTRIBUTE_VALUE = "value"

    def __init__(self, filename, fetchfn):
        # type: (str, Callable[[str], str]) -> None
        self._dom = self._normalize(filename, fetchfn)

        # indexing
        self._defaultelement = self._finddefaultelement()

    def _normalize(self, filename, fetchfn):
        # type: Callable[[str], str] ->  xml.dom.minidom.Document
        # Breadth-first traversal of all includes to apply files
        # Merge includes into unified DOM
        roottext = fetchfn(filename)
        dom = xml.dom.minidom.parseString(roottext)
        while True:
            matches = dom.getElementsByTagName(self.DOM_ELEMENT_INCLUDE)
            if not matches:
                break
            for includenode in matches:
                includename = includenode.getAttribute(self.DOM_ATTRIBUTE_NAME)
                includetext = fetchfn(includename)
                includedom = xml.dom.minidom.parseString(includetext)
                insertmanifests = includedom.getElementsByTagName(
                    self.DOM_ELEMENT_MANIFEST
                )
                if len(insertmanifests) == 0:
                    raise InvalidManifestException(
                        _('Could not find root <manifest> element in "%s"')
                        % includename
                    )
                elif len(insertmanifests) > 1:
                    raise InvalidManifestException(
                        _('Found more than one <manifest> element in "%s"')
                        % includename
                    )
                insertmanifest = insertmanifests[0]
                for node in insertmanifest.childNodes:
                    newnode = dom.importNode(node, True)
                    includenode.parentNode.insertBefore(newnode, includenode)
                includenode.parentNode.removeChild(includenode)
        return dom

    def _finddefaultelement(self):
        # type: () -> Optional[Any]
        """Finds the DOM node representing the default element."""
        matches = self._dom.getElementsByTagName(self.DOM_ELEMENT_DEFAULT)
        if len(matches) == 0:
            return None
        elif len(matches) > 1:
            raise InvalidManifestException(
                _("Found more than one default element in manifest")
            )

        return matches[0]

    def _getprojectelement(self, projectname):
        # type: (str) -> Optional[Any]
        # TODO: Index this?
        matches = [
            projectelement
            for projectelement in self._dom.getElementsByTagName(
                self.DOM_ELEMENT_PROJECT
            )
            if projectelement.getAttribute(self.DOM_ATTRIBUTE_NAME) == projectname
        ]
        return matches[-1] if matches else None

    def _getremoteelement(self, remotename):
        # type: (str) -> Optional[Any]
        matches = [
            remoteelement
            for remoteelement in self._dom.getElementsByTagName(self.DOM_ELEMENT_REMOTE)
            if remoteelement.getAttribute(self.DOM_ATTRIBUTE_NAME) == remotename
        ]
        if len(matches) == 0:
            return None
        elif len(matches) > 1:
            raise InvalidManifestException(
                _('Manifest contains multiple remotes named "%s"') % remotename
            )
        return matches[0]

    def hasproject(self, projectname):
        # type: (str) -> bool
        """Returns true if the project is defined in the manifest"""
        return self._getprojectelement(projectname) is not None

    @classmethod
    def _trygetelementattribute(cls, element, attributename):
        # type: (Optional[Any], str) -> Optional[str]
        if element is None:
            return None
        if not element.hasAttribute(attributename):
            return None
        return element.getAttribute(attributename)

    def _getprojectremotework(self, projectelement):
        # type: (Any) -> Optional[str]
        if projectelement is None:
            raise ValueError(_("projectelement may not be None"))

        attr = repomanifest.DOM_ATTRIBUTE_REMOTE
        remotename = repomanifest._trygetelementattribute(
            projectelement, attr
        ) or repomanifest._trygetelementattribute(self._defaultelement, attr)
        return remotename

    def _ishash(self, text):
        return len(text) == 40 and re.match("^[0-9a-fA-F]{40}$", text)

    def _getprojectrevisionwork(self, projectelement):
        # type: (Any) -> str
        if projectelement is None:
            raise ValueError(_("projectelement may not be None"))

        remotename = self._getprojectremotework(projectelement)
        remoteelement = (
            self._getremoteelement(remotename) if remotename is not None else None
        )

        attr = repomanifest.DOM_ATTRIBUTE_REVISION
        revision = (
            repomanifest._trygetelementattribute(projectelement, attr)
            or repomanifest._trygetelementattribute(remoteelement, attr)
            or repomanifest._trygetelementattribute(self._defaultelement, attr)
        )
        if revision is None:
            raise ValueError(
                _('No revision specified anywhere for \"%s\"')
                % projectelement.getAttribute(self.DOM_ATTRIBUTE_NAME)
            )

        if self._ishash(revision):
            return revision
        else:
            return "%s/%s" % (remotename, revision)

    def getprojects(self):
        # type: () -> List[Tuple[str, str, str]]
        projects = [
            (
                projectelement.getAttribute(self.DOM_ATTRIBUTE_NAME),
                projectelement.getAttribute(self.DOM_ATTRIBUTE_PATH),
                self._getprojectrevisionwork(projectelement),
            )
            for projectelement in self._dom.getElementsByTagName(
                self.DOM_ELEMENT_PROJECT
            )
        ]
        return projects

    def getprojectrevision(self, projectname, path=None):
        # type: (str, Union[None, str]) -> str
        """Evaluates the version specified for the project either explicitly or with a default"""
        projectelement = self._getprojectelement(projectname)
        return self._getprojectrevisionwork(projectelement)

    def getprojectpaths(self, projectname):
        # type: (str) -> List[str]
        """Finds the list of all repo paths where a project is mounted"""
        # TODO: Index this?
        matches = [
            projectelement
            for projectelement in self._dom.getElementsByTagName(
                self.DOM_ELEMENT_PROJECT
            )
            if projectelement.getAttribute(self.DOM_ATTRIBUTE_NAME) == projectname
        ]
        return [
            projectelement.getAttribute(self.DOM_ATTRIBUTE_PATH)
            for projectelement in matches
        ]

    def getprojectpathrevisions(self, projectname):
        # type: (str) -> Dict[str, str]
        """Finds the mapping of all repo paths where a project is mounted to their revisions"""
        # TODO: Index this?
        matches = [
            projectelement
            for projectelement in self._dom.getElementsByTagName(
                self.DOM_ELEMENT_PROJECT
            )
            if projectelement.getAttribute(self.DOM_ATTRIBUTE_NAME) == projectname
        ]
        return {
            projectelement.getAttribute(
                self.DOM_ATTRIBUTE_PATH
            ): self._getprojectrevisionwork(projectelement)
            for projectelement in matches
        }

    def getprojectnameforpath(self, projectpath):
        # type: (str) -> Optional[str]
        """Get the name of the Git project located at a particular repo path"""
        # TODO: Path normalization
        matches = [
            projectelement
            for projectelement in self._dom.getElementsByTagName(
                self.DOM_ELEMENT_PROJECT
            )
            if projectelement.getAttribute(self.DOM_ATTRIBUTE_PATH) == projectpath
        ]
        return matches[-1] if matches else None

    def geturiforproject(self, projectname):
        # type: (str) -> str
        """Constructs the URI"""
        projectelement = self._getprojectelement(projectname)
        if projectelement is None:
            raise LookupError(_('Could not find project "%s"') % projectname)

        remotename = self._getprojectremotework(projectelement)
        if remotename is None:
            raise InvalidManifestException(
                _('Project element "%s" does not have a remote') % projectname
            )

        remoteelement = self._getremoteelement(remotename)
        if remoteelement is None:
            raise InvalidManifestException(
                _('Could not find remote "%s" in the manifest') % remotename
            )

        baseuri = remoteelement.getAttribute(self.DOM_ATTRIBUTE_FETCH)
        uri = baseuri + "/" + projectname
        return uri
