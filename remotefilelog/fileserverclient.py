# fileserverclient.py - client for communicating with the cache process
#
# Copyright 2013 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from mercurial.i18n import _
from mercurial.node import hex, bin, nullid
from mercurial import util, sshpeer, hg, error, util, wireproto, httppeer
from mercurial import extensions
import hashlib, os, lz4, time, io, struct
import itertools
import threading

import constants, datapack, historypack, shallowutil
from shallowutil import readexactly, readunpack

# Statistics for debugging
fetchcost = 0
fetches = 0
fetched = 0
fetchmisses = 0

_downloading = _('downloading')

def getcachekey(reponame, file, id):
    pathhash = hashlib.sha1(file).hexdigest()
    return os.path.join(reponame, pathhash[:2], pathhash[2:], id)

def getlocalkey(file, id):
    pathhash = hashlib.sha1(file).hexdigest()
    return os.path.join(pathhash, id)

def peersetup(ui, peer):
    class remotefilepeer(peer.__class__):
        @wireproto.batchable
        def getfile(self, file, node):
            if not self.capable('getfile'):
                raise error.Abort(
                    'configured remotefile server does not support getfile')
            f = wireproto.future()
            yield {'file': file, 'node': node}, f
            code, data = f.value.split('\0', 1)
            if int(code):
                raise error.LookupError(file, node, data)
            yield data

        @wireproto.batchable
        def getflogheads(self, path):
            if not self.capable('getflogheads'):
                raise error.Abort('configured remotefile server does not '
                                  'support getflogheads')
            f = wireproto.future()
            yield {'path': path}, f
            heads = f.value.split('\n') if f.value else []
            yield heads

        def _updatecallstreamopts(self, command, opts):
            if command != 'getbundle':
                return
            if 'remotefilelog' not in self._capabilities():
                return
            if not util.safehasattr(self, '_localrepo'):
                return
            if constants.REQUIREMENT not in self._localrepo.requirements:
                return

            bundlecaps = opts.get('bundlecaps')
            if bundlecaps:
                bundlecaps = [bundlecaps]
            else:
                bundlecaps = []

            # shallow, includepattern, and excludepattern are a hacky way of
            # carrying over data from the local repo to this getbundle
            # command. We need to do it this way because bundle1 getbundle
            # doesn't provide any other place we can hook in to manipulate
            # getbundle args before it goes across the wire. Once we get rid
            # of bundle1, we can use bundle2's _pullbundle2extraprepare to
            # do this more cleanly.
            bundlecaps.append('remotefilelog')
            if self._localrepo.includepattern:
                patterns = '\0'.join(self._localrepo.includepattern)
                includecap = "includepattern=" + patterns
                bundlecaps.append(includecap)
            if self._localrepo.excludepattern:
                patterns = '\0'.join(self._localrepo.excludepattern)
                excludecap = "excludepattern=" + patterns
                bundlecaps.append(excludecap)
            opts['bundlecaps'] = ','.join(bundlecaps)

        def _callstream(self, command, **opts):
            self._updatecallstreamopts(command, opts)
            return super(remotefilepeer, self)._callstream(command, **opts)

    peer.__class__ = remotefilepeer

class cacheconnection(object):
    """The connection for communicating with the remote cache. Performs
    gets and sets by communicating with an external process that has the
    cache-specific implementation.
    """
    def __init__(self):
        self.pipeo = self.pipei = self.pipee = None
        self.subprocess = None
        self.connected = False

    def connect(self, cachecommand):
        if self.pipeo:
            raise error.Abort(_("cache connection already open"))
        self.pipei, self.pipeo, self.pipee, self.subprocess = \
            util.popen4(cachecommand)
        self.connected = True

    def close(self):
        def tryclose(pipe):
            try:
                pipe.close()
            except Exception:
                pass
        if self.connected:
            try:
                self.pipei.write("exit\n")
            except Exception:
                pass
            tryclose(self.pipei)
            self.pipei = None
            tryclose(self.pipeo)
            self.pipeo = None
            tryclose(self.pipee)
            self.pipee = None
            try:
                # Wait for process to terminate, making sure to avoid deadlock.
                # See https://docs.python.org/2/library/subprocess.html for
                # warnings about wait() and deadlocking.
                self.subprocess.communicate()
            except Exception:
                pass
            self.subprocess = None
        self.connected = False

    def request(self, request, flush=True):
        if self.connected:
            try:
                self.pipei.write(request)
                if flush:
                    self.pipei.flush()
            except IOError:
                self.close()

    def receiveline(self):
        if not self.connected:
            return None
        try:
            result = self.pipeo.readline()[:-1]
            if not result:
                self.close()
        except IOError:
            self.close()

        return result

def _getfilesbatch(
        remote, receivemissing, progresstick, missed, idmap, batchsize):
    # Over http(s), iterbatch is a streamy method and we can start
    # looking at results early. This means we send one (potentially
    # large) request, but then we show nice progress as we process
    # file results, rather than showing chunks of $batchsize in
    # progress.
    #
    # Over ssh, iterbatch isn't streamy because batch() wasn't
    # explicitly designed as a streaming method. In the future we
    # should probably introduce a streambatch() method upstream and
    # use that for this.
    if (getattr(remote, 'iterbatch', False) and remote.capable('httppostargs')
        and isinstance(remote, httppeer.httppeer)):
        b = remote.iterbatch()
        for m in missed:
            file_ = idmap[m]
            node = m[-40:]
            b.getfile(file_, node)
        b.submit()
        for m, r in itertools.izip(missed, b.results()):
            file_ = idmap[m]
            node = m[-40:]
            receivemissing(io.BytesIO('%d\n%s' % (len(r), r)), file_, node)
            progresstick()
        return
    while missed:
        chunk, missed = missed[:batchsize], missed[batchsize:]
        b = remote.batch()
        futures = {}
        for m in chunk:
            file_ = idmap[m]
            node = m[-40:]
            futures[m] = b.getfile(file_, node)
        b.submit()
        for m in chunk:
            v = futures[m].value
            file_ = idmap[m]
            node = m[-40:]
            receivemissing(io.BytesIO('%d\n%s' % (len(v), v)), file_, node)
            progresstick()

def _getfiles(
    remote, receivemissing, progresstick, missed, idmap):
    i = 0
    while i < len(missed):
        # issue a batch of requests
        start = i
        end = min(len(missed), start + 10000)
        i = end

        def worker():
            try:
                # issue new request
                for missingid in missed[start:end]:
                    if worker.stop:
                        # This allows to gracefully exit the workerthread if an
                        # exception was raised in the main thread.
                        return
                    versionid = missingid[-40:]
                    file = idmap[missingid]
                    sshrequest = "%s%s\n" % (versionid, file)
                    remote.pipeo.write(sshrequest)
                    # worker.requested is used to inform the main thread that
                    # a request have been processed and that it can try
                    # receive the result. This is to avoid a deadlock if an
                    # exception was raised in the worker and the main thread
                    # was already trying to read from the pipe.
                    worker.requested += 1
                remote.pipeo.flush()
            except Exception as e:
                # Set the exception so that the main thread can re-raise it
                # later
                worker.exception = e
                return

        # I use function attributes to share data between threads.
        # This allows a simpler implementation than using events, queues
        # and locks. Considering this worker is scoped to this function,
        # race conditions are managed by making sure a single thread writes
        # to a variable. I also make sure the logic reading a variable does not
        # depend on the atomicity of write operations
        worker.exception = None  # Written in worker, read in main thread
        worker.requested = -1  # Written in worker, read in main thread
        worker.stop = False  # Written in main thread, read in worker

        workerthread = threading.Thread(target=worker)
        # Normally, the workerthread should always be gracefully exited by the
        # main thread if an exception is raised. Setting the thread daemon
        # should not be required, but I let it there as a safety net. Maybe it
        # could be removed.
        workerthread.daemon = True
        workerthread.start()

        try:
            # receive batch results
            for n, missingid in enumerate(missed[start:end]):
                while n > worker.requested:
                    # This while loop allows to wait for a request to have been
                    # properly issued by the workerthread. In the meantime, if
                    # an exception is raised in the workerthread, it is
                    # immediately re-raised in the main thread. This is to avoid
                    # a deadlock in case the main thread is expecting data in
                    # remote.pipei and the worker thread will never issue the
                    # request because it raised an exception.
                    if worker.exception is not None:
                        raise worker.exception
                versionid = missingid[-40:]
                file = idmap[missingid]
                receivemissing(remote.pipei, file, versionid)
                progresstick()
        except BaseException:
            # Gracefully exit the workerthread before raising the exception
            worker.stop = True
            raise

        # The workerthread should always be done here and join might not be
        # necessary...
        workerthread.join()

class fileserverclient(object):
    """A client for requesting files from the remote file server.
    """
    def __init__(self, repo):
        ui = repo.ui
        self.repo = repo
        self.ui = ui
        self.cacheprocess = ui.config("remotefilelog", "cacheprocess")
        if self.cacheprocess:
            self.cacheprocess = util.expandpath(self.cacheprocess)


        # This option causes remotefilelog to pass the full file path to the
        # cacheprocess instead of a hashed key.
        self.cacheprocesspasspath = ui.configbool(
            "remotefilelog", "cacheprocess.includepath")

        self.debugoutput = ui.configbool("remotefilelog", "debug")

        self.remotecache = cacheconnection()
        self.remoteserver = None

    def setstore(self, datastore, historystore, writedata, writehistory):
        self.datastore = datastore
        self.historystore = historystore
        self.writedata = writedata
        self.writehistory = writehistory

    def _connect(self):
        fallbackpath = self.repo.fallbackpath
        needcreate = False
        if not self.remoteserver:
            if not fallbackpath:
                raise error.Abort("no remotefilelog server "
                    "configured - is your .hg/hgrc trusted?")
            needcreate = True
        elif (isinstance(self.remoteserver, sshpeer.sshpeer) and
                 self.remoteserver.subprocess.poll() is not None):
            # The ssh connection died, so recreate it.
            needcreate = True

        def _cleanup(orig):
            # close pipee first so peer.cleanup reading it won't deadlock, if
            # there are other processes with pipeo open (i.e. us).
            peer = orig.im_self
            if util.safehasattr(peer, 'pipee'):
                peer.pipee.close()
            return orig()

        if needcreate:
            peer = hg.peer(self.ui, {}, fallbackpath)
            if util.safehasattr(peer, 'cleanup'):
                extensions.wrapfunction(peer, 'cleanup', _cleanup)
            self.remoteserver = peer

        return self.remoteserver

    def request(self, fileids):
        """Takes a list of filename/node pairs and fetches them from the
        server. Files are stored in the local cache.
        A list of nodes that the server couldn't find is returned.
        If the connection fails, an exception is raised.
        """
        if not self.remotecache.connected:
            self.connect()
        cache = self.remotecache
        writedata = self.writedata

        if self.ui.configbool('remotefilelog', 'fetchpacks'):
            self.requestpack(fileids)
            return

        repo = self.repo
        count = len(fileids)
        request = "get\n%d\n" % count
        idmap = {}
        reponame = repo.name
        for file, id in fileids:
            fullid = getcachekey(reponame, file, id)
            if self.cacheprocesspasspath:
                request += file + '\0'
            request += fullid + "\n"
            idmap[fullid] = file

        cache.request(request)

        missing = []
        total = count
        self.ui.progress(_downloading, 0, total=count)

        missed = []
        count = 0
        while True:
            missingid = cache.receiveline()
            if not missingid:
                missedset = set(missed)
                for missingid in idmap.iterkeys():
                    if not missingid in missedset:
                        missed.append(missingid)
                self.ui.warn(_("warning: cache connection closed early - " +
                    "falling back to server\n"))
                break
            if missingid == "0":
                break
            if missingid.startswith("_hits_"):
                # receive progress reports
                parts = missingid.split("_")
                count += int(parts[2])
                self.ui.progress(_downloading, count, total=total)
                continue

            missed.append(missingid)

        global fetchmisses
        fetchmisses += len(missed)

        count = [total - len(missed)]
        self.ui.progress(_downloading, count[0], total=total)
        self.ui.log("remotefilelog", "remote cache hit rate is %r of %r ",
                    count[0], total, hit=count[0], total=total)

        oldumask = os.umask(0o002)
        try:
            # receive cache misses from master
            if missed:
                def progresstick():
                    count[0] += 1
                    self.ui.progress(_downloading, count[0], total=total)
                # When verbose is true, sshpeer prints 'running ssh...'
                # to stdout, which can interfere with some command
                # outputs
                verbose = self.ui.verbose
                self.ui.verbose = False
                try:
                    remote = self._connect()

                    # TODO: deduplicate this with the constant in shallowrepo
                    if remote.capable("remotefilelog"):
                        if not isinstance(remote, sshpeer.sshpeer):
                            raise error.Abort('remotefilelog requires ssh '
                                              'servers')
                        # If it's a new connection, issue the getfiles command
                        if not getattr(remote, '_getfilescalled', False):
                            remote._callstream("getfiles")
                            remote._getfilescalled = True
                        _getfiles(remote, self.receivemissing, progresstick,
                                  missed, idmap)
                    elif remote.capable("getfile"):
                        batchdefault = 100 if remote.capable('batch') else 10
                        batchsize = self.ui.configint(
                            'remotefilelog', 'batchsize', batchdefault)
                        _getfilesbatch(
                            remote, self.receivemissing, progresstick, missed,
                            idmap, batchsize)
                    else:
                        raise error.Abort("configured remotefilelog server"
                                         " does not support remotefilelog")
                finally:
                    self.ui.verbose = verbose
                # send to memcache
                count[0] = len(missed)
                request = "set\n%d\n%s\n" % (count[0], "\n".join(missed))
                cache.request(request)

            self.ui.progress(_downloading, None)

            # mark ourselves as a user of this cache
            writedata.markrepo(self.repo.path)
        finally:
            os.umask(oldumask)

        return missing

    def endrequest(self):
        """End the getfiles request loop.

        It's useful if we want to run other commands using the same sshpeer.
        """
        remote = self.remoteserver
        if remote is None:
            return
        if not getattr(remote, '_getfilescalled', False):
            return
        remote.pipeo.write('\n')
        remote.pipeo.flush()
        remote._getfilescalled = False

    def receivemissing(self, pipe, filename, node):
        line = pipe.readline()[:-1]
        if not line:
            raise error.ResponseError(_("error downloading file contents:"),
                                      _("connection closed early"))
        size = int(line)
        data = pipe.read(size)
        if len(data) != size:
            raise error.ResponseError(_("error downloading file contents:"),
                                      _("only received %s of %s bytes")
                                      % (len(data), size))

        self.writedata.addremotefilelognode(filename, bin(node),
                                             lz4.decompress(data))

    def requestpack(self, fileids):
        """Requests the given file revisions from the server in a pack format.

        See `remotefilelogserver.getpack` for the file format.
        """
        remote = self._connect()
        remote._callstream("getpackv1")

        groupedfiles = self._sendpackrequest(remote, fileids)

        i = 0
        self.ui.progress(_downloading, i, total=len(groupedfiles))

        packpath = shallowutil.getcachepackpath(self.repo,
                                                constants.FILEPACK_CATEGORY)
        shallowutil.mkstickygroupdir(self.repo.ui, packpath)

        with datapack.mutabledatapack(self.ui, packpath) as dpack:
            with historypack.mutablehistorypack(self.ui, packpath) as hpack:
                for filename in self.readfiles(remote):
                    i += 1
                    self.ui.progress(_downloading, i, total=len(groupedfiles))
                    for value in self.readhistory(remote):
                        node, p1, p2, linknode, copyfrom = value
                        hpack.add(filename, node, p1, p2, linknode, copyfrom)

                    for node, deltabase, delta in self.readdeltas(remote):
                        dpack.add(filename, node, deltabase, delta)

        self.ui.progress(_downloading, None)

    def _sendpackrequest(self, remote, fileids):
        """Formats and writes the given fileids to the remote as part of a
        getpackv1 call.
        """
        # Sort the requests by name, so we receive requests in batches by name
        grouped = {}
        for filename, node in fileids:
            grouped.setdefault(filename, set()).add(node)

        # Issue request
        for filename, nodes in grouped.iteritems():
            filenamelen = struct.pack(constants.FILENAMESTRUCT, len(filename))
            countlen = struct.pack(constants.PACKREQUESTCOUNTSTRUCT, len(nodes))
            rawnodes = ''.join(bin(n) for n in nodes)

            remote.pipeo.write('%s%s%s%s' % (filenamelen, filename, countlen,
                                             rawnodes))
            remote.pipeo.flush()
        remote.pipeo.write(struct.pack(constants.FILENAMESTRUCT, 0))
        remote.pipeo.flush()

        return grouped

    def readfiles(self, remote):
        while True:
            filenamelen = readunpack(remote.pipei, constants.FILENAMESTRUCT)[0]
            if filenamelen == 0:
                break
            yield readexactly(remote.pipei, filenamelen)

    def readhistory(self, remote):
        count = readunpack(remote.pipei, '!I')[0]
        for i in xrange(count):
            entry = readunpack(remote.pipei,'!20s20s20s20sH')
            if entry[4] != 0:
                copyfrom = readexactly(remote.pipei, entry[4])
            else:
                copyfrom = ''
            entry = entry[:4] + (copyfrom,)
            yield entry

    def readdeltas(self, remote):
        count = readunpack(remote.pipei, '!I')[0]
        for i in xrange(count):
            node, deltabase, deltalen = readunpack(remote.pipei, '!20s20sQ')
            delta = readexactly(remote.pipei, deltalen)
            yield (node, deltabase, delta)

    def connect(self):
        if self.cacheprocess:
            cmd = "%s %s" % (self.cacheprocess, self.writedata._path)
            self.remotecache.connect(cmd)
        else:
            # If no cache process is specified, we fake one that always
            # returns cache misses.  This enables tests to run easily
            # and may eventually allow us to be a drop in replacement
            # for the largefiles extension.
            class simplecache(object):
                def __init__(self):
                    self.missingids = []
                    self.connected = True

                def close(self):
                    pass

                def request(self, value, flush=True):
                    lines = value.split("\n")
                    if lines[0] != "get":
                        return
                    self.missingids = lines[2:-1]
                    self.missingids.append('0')

                def receiveline(self):
                    if len(self.missingids) > 0:
                        return self.missingids.pop(0)
                    return None

            self.remotecache = simplecache()

    def close(self):
        if fetches and self.debugoutput:
            self.ui.warn(("%s files fetched over %d fetches - " +
                "(%d misses, %0.2f%% hit ratio) over %0.2fs\n") % (
                    fetched,
                    fetches,
                    fetchmisses,
                    float(fetched - fetchmisses) / float(fetched) * 100.0,
                    fetchcost))

        if self.remotecache.connected:
            self.remotecache.close()

        if self.remoteserver and util.safehasattr(self.remoteserver, 'cleanup'):
            self.remoteserver.cleanup()
            self.remoteserver = None

    def prefetch(self, fileids, force=False, fetchdata=True,
                 fetchhistory=False):
        """downloads the given file versions to the cache
        """
        repo = self.repo
        idstocheck = []
        for file, id in fileids:
            # hack
            # - we don't use .hgtags
            # - workingctx produces ids with length 42,
            #   which we skip since they aren't in any cache
            if (file == '.hgtags' or len(id) == 42
                or not repo.shallowmatch(file)):
                continue

            idstocheck.append((file, bin(id)))

        datastore = self.datastore
        historystore = self.historystore
        if force:
            datastore = self.writedata
            historystore = self.writehistory

        missingids = set()
        if fetchdata:
            missingids.update(datastore.getmissing(idstocheck))
        if fetchhistory:
            missingids.update(historystore.getmissing(idstocheck))

        # partition missing nodes into nullid and not-nullid so we can
        # warn about this filtering potentially shadowing bugs.
        nullids = len([None for unused, id in missingids if id == nullid])
        if nullids:
            missingids = [(f, id) for f, id in missingids if id != nullid]
            repo.ui.develwarn(
                ('remotefilelog not fetching %d null revs'
                 ' - this is likely hiding bugs' % nullids),
                config='remotefilelog-ext')
        if missingids:
            global fetches, fetched, fetchcost
            fetches += 1

            # We want to be able to detect excess individual file downloads, so
            # let's log that information for debugging.
            if fetches >= 15 and fetches < 18:
                if fetches == 15:
                    fetchwarning = self.ui.config('remotefilelog',
                                                  'fetchwarning')
                    if fetchwarning:
                        self.ui.warn(fetchwarning + '\n')
                self.logstacktrace()
            missingids = [(file, hex(id)) for file, id in missingids]
            fetched += len(missingids)
            start = time.time()
            missingids = self.request(missingids)
            if missingids:
                raise error.Abort(_("unable to download %d files") %
                                  len(missingids))
            fetchcost += time.time() - start

    def logstacktrace(self):
        import traceback
        self.ui.log('remotefilelog', 'excess remotefilelog fetching:\n%s',
                    ''.join(traceback.format_stack()))
