# shallowutil.py -- remotefilelog utilities
#
# Copyright 2014 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

import errno, hashlib, os, stat, struct, tempfile
from mercurial import filelog, util, error
from mercurial.i18n import _

import constants

if os.name != 'nt':
    import grp

def interposeclass(container, classname):
    '''Interpose a class into the hierarchies of all loaded subclasses. This
    function is intended for use as a decorator.

      import mymodule
      @replaceclass(mymodule, 'myclass')
      class mysubclass(mymodule.myclass):
          def foo(self):
              f = super(mysubclass, self).foo()
              return f + ' bar'

    Existing instances of the class being replaced will not have their
    __class__ modified, so call this function before creating any
    objects of the target type. Note that this doesn't actually replace the
    class in the module -- that can cause problems when using e.g. super()
    to call a method in the parent class. Instead, new instances should be
    created using a factory of some sort that this extension can override.
    '''
    def wrap(cls):
        oldcls = getattr(container, classname)
        oldbases = (oldcls,)
        newbases = (cls,)
        for subcls in oldcls.__subclasses__():
            if subcls is not cls:
                assert subcls.__bases__ == oldbases
                subcls.__bases__ = newbases
        return cls
    return wrap

def getcachekey(reponame, file, id):
    pathhash = hashlib.sha1(file).hexdigest()
    return os.path.join(reponame, pathhash[:2], pathhash[2:], id)

def getlocalkey(file, id):
    pathhash = hashlib.sha1(file).hexdigest()
    return os.path.join(pathhash, id)

def getcachepath(ui, allowempty=False):
    cachepath = ui.config("remotefilelog", "cachepath")
    if not cachepath:
        if allowempty:
            return None
        else:
            raise error.Abort(_("could not find config option "
                                "remotefilelog.cachepath"))
    return util.expandpath(cachepath)

def getcachepackpath(repo, category):
    cachepath = getcachepath(repo.ui)
    if category != constants.FILEPACK_CATEGORY:
        return os.path.join(cachepath, repo.name, 'packs', category)
    else:
        return os.path.join(cachepath, repo.name, 'packs')

def getlocalpackpath(base, category):
    return os.path.join(base, 'packs', category)

def createrevlogtext(text, copyfrom=None, copyrev=None):
    """returns a string that matches the revlog contents in a
    traditional revlog
    """
    meta = {}
    if copyfrom or text.startswith('\1\n'):
        if copyfrom:
            meta['copy'] = copyfrom
            meta['copyrev'] = copyrev
        text = filelog.packmeta(meta, text)

    return text

def parsemeta(text):
    meta, size = filelog.parsemeta(text)
    if text.startswith('\1\n'):
        s = text.index('\1\n', 2)
        text = text[s + 2:]
    return meta or {}, text

def parsesize(raw):
    try:
        index = raw.index('\0')
        size = int(raw[:index])
    except ValueError:
        raise RuntimeError("corrupt cache data")
    return index, size

def ancestormap(raw):
    index, size = parsesize(raw)
    start = index + 1 + size

    mapping = {}
    while start < len(raw):
        divider = raw.index('\0', start + 80)

        currentnode = raw[start:(start + 20)]
        p1 = raw[(start + 20):(start + 40)]
        p2 = raw[(start + 40):(start + 60)]
        linknode = raw[(start + 60):(start + 80)]
        copyfrom = raw[(start + 80):divider]

        mapping[currentnode] = (p1, p2, linknode, copyfrom)
        start = divider + 1

    return mapping

def readfile(path):
    f = open(path, 'rb')
    try:
        result = f.read()

        # we should never have empty files
        if not result:
            os.remove(path)
            raise IOError("empty file: %s" % path)

        return result
    finally:
        f.close()


def unlinkfile(filepath):
    if os.name == 'nt':
        # On Windows, os.unlink cannnot delete readonly files
        os.chmod(filepath, stat.S_IWUSR)

    os.unlink(filepath)


def renamefile(source, destination):
    if os.name == 'nt':
        # On Windows, os.rename cannot rename readonly files
        # and cannot overwrite destination if it exists
        os.chmod(source, stat.S_IWUSR)
        if os.path.isfile(destination):
            os.chmod(destination, stat.S_IWUSR)
            os.unlink(destination)

    os.rename(source, destination)


def writefile(path, content, readonly=False):
    dirname, filename = os.path.split(path)
    if not os.path.exists(dirname):
        try:
            os.makedirs(dirname)
        except OSError as ex:
            if ex.errno != errno.EEXIST:
                raise

    fd, temp = tempfile.mkstemp(prefix='.%s-' % filename, dir=dirname)
    os.close(fd)

    try:
        f = util.posixfile(temp, 'wb')
        f.write(content)
        f.close()

        if readonly:
            mode = 0o444
        else:
            # tempfiles are created with 0o600, so we need to manually set the
            # mode.
            oldumask = os.umask(0)
            # there's no way to get the umask without modifying it, so set it
            # back
            os.umask(oldumask)
            mode = ~oldumask

        renamefile(temp, path)
        os.chmod(path, mode)
    except Exception:
        try:
            unlinkfile(temp)
        except OSError:
            pass
        raise

def sortnodes(nodes, parentfunc):
    """Topologically sorts the nodes, using the parentfunc to find
    the parents of nodes."""
    nodes = set(nodes)
    childmap = {}
    parentmap = {}
    roots = []

    # Build a child and parent map
    for n in nodes:
        parents = [p for p in parentfunc(n) if p in nodes]
        parentmap[n] = set(parents)
        for p in parents:
            childmap.setdefault(p, set()).add(n)
        if not parents:
            roots.append(n)

    # Process roots, adding children to the queue as they become roots
    results = []
    while roots:
        n = roots.pop(0)
        results.append(n)
        if n in childmap:
            children = childmap[n]
            for c in children:
                childparents = parentmap[c]
                childparents.remove(n)
                if len(childparents) == 0:
                    # insert at the beginning, that way child nodes
                    # are likely to be output immediately after their
                    # parents.  This gives better compression results.
                    roots.insert(0, c)

    return results

def readexactly(stream, n):
    '''read n bytes from stream.read and abort if less was available'''
    s = stream.read(n)
    if len(s) < n:
        raise error.Abort(_("stream ended unexpectedly"
                           " (got %d bytes, expected %d)")
                          % (len(s), n))
    return s

def readunpack(stream, fmt):
    data = readexactly(stream, struct.calcsize(fmt))
    return struct.unpack(fmt, data)

def getgid(groupname):
    try:
        gid = grp.getgrnam(groupname).gr_gid
        return gid
    except KeyError:
        return None

def setstickygroupdir(path, gid, warn=None):
    if gid is None:
        return
    try:
        os.chown(path, -1, gid)
        os.chmod(path, 0o2775)
    except (IOError, OSError) as ex:
        if warn:
            warn(_('unable to chown/chmod on %s: %s\n') % (path, ex))

def mkstickygroupdir(ui, path):
    """Creates the given directory (if it doesn't exist) and give it a
    particular group with setgid enabled."""
    gid = None
    groupname = ui.config("remotefilelog", "cachegroup")
    if groupname:
        gid = getgid(groupname)
        if gid is None:
            ui.warn(_('unable to resolve group name: %s\n') % groupname)

    # we use a single stat syscall to test the existence and mode / group bit
    st = None
    try:
        st = os.stat(path)
    except OSError:
        pass

    if st:
        # exists
        if (st.st_mode & 0o2775) != 0o2775 or st.st_gid != gid:
            # permission needs to be fixed
            setstickygroupdir(path, gid, ui.warn)
        return

    oldumask = os.umask(0o002)
    try:
        missingdirs = [path]
        path = os.path.dirname(path)
        while path and not os.path.exists(path):
            missingdirs.append(path)
            path = os.path.dirname(path)

        for path in reversed(missingdirs):
            os.mkdir(path)

        for path in missingdirs:
            setstickygroupdir(path, gid, ui.warn)
    finally:
        os.umask(oldumask)
