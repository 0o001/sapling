# Copyright 2014 Facebook Inc.
#
"""upload useful diagnostics and give instructions for asking for help

    [rage]
    # Name of the rpm binary
    rpmbin = rpm
"""
import subprocess

from functools import partial
from mercurial.i18n import _
from mercurial import (
    bookmarks,
    cmdutil,
    commands,
    encoding,
    error,
    progress,
    pycompat,
    registrar,
    scmutil,
    util)

from . import (
    shareutil,
)
import os, socket, re, tempfile, time, traceback

from .remotefilelog import (
    constants,
    shallowutil
)

cmdtable = {}
command = registrar.command(cmdtable)
_failsafeerrors = []

def _failsafe(func):
    try:
        return func()
    except Exception as ex:
        index = len(_failsafeerrors) + 1
        message = "[%d]: %s\n%s\n" % (index, str(ex), traceback.format_exc())
        _failsafeerrors.append(message)
        return '(Failed. See footnote [%d])' % index

def shcmd(cmd, input=None, check=True, keeperr=True):
    _, _, _, p = util.popen4(cmd)
    out, err = p.communicate(input)
    if check and p.returncode:
        raise error.Abort(cmd + ' error: ' + err)
    elif keeperr:
        out += err
    return out

def which(name):
    """ """
    for p in encoding.environ.get('PATH', '/bin').split(pycompat.ospathsep):
        path = os.path.join(p, name)
        if os.path.exists(path):
            return path
    return None

rageopts = [('p', 'preview', None,
             _('print diagnostic information without doing arc paste'))]
if which('oncalls'):
    rageopts.append(('', 'oncall', None,
                     _('file a task for source control oncall')))

def localconfig(ui):
    result = []
    for section, name, value in ui.walkconfig():
        source = ui.configsource(section, name)
        if source.find('/etc/') == -1 and source.find('/default.d/') == -1:
            result.append('%s.%s=%s  # %s' % (section, name, value, source))
    return result

def obsoleteinfo(repo, hgcmd):
    """Return obsolescence markers that are relevant to smartlog revset"""
    unfi = repo.unfiltered()
    revs = scmutil.revrange(unfi, ["smartlog()"])
    hashes = '|'.join(unfi[rev].hex() for rev in revs)
    markers = hgcmd('debugobsolete', rev=[])
    pat = re.compile('(^.*(?:'+hashes+').*$)', re.MULTILINE)
    relevant = pat.findall(markers)
    return "\n".join(relevant)

def usechginfo():
    """FBONLY: Information about whether chg is enabled"""
    files = {
        'system': '/etc/mercurial/usechg',
        'user': os.path.expanduser('~/.usechg'),
    }
    result = []
    for name, path in files.items():
        if os.path.exists(path):
            with open(path) as f:
                value = f.read().strip()
        else:
            value = '(not set)'
        result.append('%s: %s' % (name, value))
    return '\n'.join(result)

def rpminfo(ui):
    """FBONLY: Information about RPM packages"""
    result = set()
    rpmbin = ui.config('rage', 'rpmbin', 'rpm')
    for name in ['hg', 'hg.real']:
        path = which(name)
        if not path:
            continue
        result.add(shcmd('%s -qf %s' % (rpmbin, path), check=False))
    return ''.join(result)

def infinitepushbackuplogs(ui, repo):
    """Contents of recent infinitepush log files."""
    logdir = ui.config('infinitepushbackup', 'logdir')
    if not logdir:
        return "infinitepushbackup.logdir not set"
    try:
        username = util.shortuser(ui.username())
    except Exception:
        username = 'unknown'
    userlogdir = os.path.join(logdir, username)
    if not os.path.exists(userlogdir):
        return "log directory does not exist: %s" % userlogdir

    # Log filenames are the reponame with the date (YYYYMMDD) appended.
    reponame = os.path.basename(repo.origroot)
    logfiles = sorted([f for f in os.listdir(userlogdir)
                       if f[:-8] == reponame])
    if not logfiles:
        return "no log files found for %s in %s" % (reponame, userlogdir)

    # Display the last 100 lines from the most recent log files.
    logs = []
    linelimit = 100
    for logfile in reversed(logfiles):
        loglines = open(os.path.join(userlogdir, logfile)).readlines()
        linecount = len(loglines)
        if linecount > linelimit:
            logcontent = '  '.join(loglines[-linelimit:])
            logs.append("%s (first %s lines omitted):\n  %s\n"
                        % (logfile, linecount - linelimit, logcontent))
            break
        else:
            logcontent = '  '.join(loglines)
            logs.append("%s:\n  %s\n" % (logfile, logcontent))
            linelimit -= linecount
    return ''.join(reversed(logs))

def readfsmonitorstate(repo):
    file = repo.vfs('fsmonitor.state', 'rb')
    return file.read()

def _makerage(ui, repo, **opts):
    srcrepo = shareutil.getsrcrepo(repo)
    def format(pair, basic=True):
        if basic:
            fmt = "%s: %s\n"
        else:
            fmt =  "%s:\n---------------------------\n%s\n"
        return fmt % pair

    def hgcmd(cmdname, *args, **additional_opts):
        cmd, opts = cmdutil.getcmdanddefaultopts(cmdname, commands.table)
        opts.update(additional_opts)

        _repo = repo
        if '_repo' in opts:
            _repo = opts['_repo']
            del opts['_repo']
        ui.pushbuffer(error=True)
        try:
            cmd(ui, _repo, *args, **opts)
        finally:
            return ui.popbuffer()

    def hgsrcrepofile(filename):
        if srcrepo.vfs.exists(filename):
            return srcrepo.vfs(filename).read()
        else:
            return "File not found: %s" % srcrepo.vfs.join(filename)

    if opts.get('oncall') and opts.get('preview'):
        raise error.Abort('--preview and --oncall cannot be used together')

    basic = [
        ('date', time.ctime()),
        ('unixname', encoding.environ.get('LOGNAME')),
        ('hostname', socket.gethostname()),
        ('repo location', _failsafe(lambda: repo.root)),
        ('active bookmark',
            _failsafe(lambda: bookmarks._readactive(repo, repo._bookmarks))),
        ('hg version', _failsafe(
            lambda: __import__('mercurial.__version__').__version__.version)),
        ('obsstore size', _failsafe(
            lambda: str(srcrepo.vfs.stat('store/obsstore').st_size))),
    ]

    oldcolormode = ui._colormode
    ui._colormode = None

    detailed = [
        ('df -h', _failsafe(lambda: shcmd('df -h', check=False))),
        ('hg sl (filtered)',
            _failsafe(lambda: hgcmd('smartlog', template='{hsl}'))),
        # unfiltered smartlog for recent hidden changesets, including full
        # node identity
        ('hg sl (unfiltered)', _failsafe(lambda: hgcmd(
            'smartlog', _repo=repo.unfiltered(), template='{node}\n{hsl}'))),
        # smartlog as the user sees it
        ('first 20 lines of "hg status"',
            _failsafe(lambda: '\n'.join(hgcmd('status').splitlines()[:20]))),
        ('hg blackbox -l60', _failsafe(lambda: hgcmd('blackbox', limit=60))),
        ('hg summary', _failsafe(lambda: hgcmd('summary'))),
        ('hg config (local)', _failsafe(lambda: '\n'.join(localconfig(ui)))),
        ('hg sparse', _failsafe(lambda: hgcmd('sparse'))),
        ('usechg', _failsafe(usechginfo)),
        ('rpm info', _failsafe(partial(rpminfo, ui))),
        ('klist', _failsafe(lambda: shcmd('klist', check=False))),
        ('ifconfig', _failsafe(lambda: shcmd('ifconfig'))),
        ('airport', _failsafe(
            lambda: shcmd('/System/Library/PrivateFrameworks/Apple80211.' +
                          'framework/Versions/Current/Resources/airport ' +
                          '--getinfo', check=False))),
        ('hg debugobsolete <smartlog>',
            _failsafe(lambda: obsoleteinfo(repo, hgcmd))),
        ('infinitepush backup state',
            _failsafe(lambda: hgsrcrepofile('infinitepushbackupstate'))),
        ('infinitepush backup logs',
            _failsafe(lambda: infinitepushbackuplogs(ui, repo))),
        ('hg config (all)', _failsafe(lambda: hgcmd('config'))),
        ('fsmonitor state', _failsafe(lambda: readfsmonitorstate(repo)))
    ]

    msg = ''

    if util.safehasattr(repo, 'name'):
        # Add the contents of both local and shared pack directories.
        packlocs = {
            'local': lambda category: shallowutil.getlocalpackpath(
                repo.svfs.vfs.base, category),
            'shared': lambda category: shallowutil.getcachepackpath(repo,
                category),
        }

        for loc, getpath in packlocs.iteritems():
            for category in constants.ALL_CATEGORIES:
                path = getpath(category)
                detailed.append((
                    "%s packs (%s)" % (loc, constants.getunits(category)),
                    "%s:\n%s" %
                    (path, _failsafe(lambda: shcmd("ls -lhS %s" % path)))
                ))

    # This is quite slow, so we don't want to do it by default
    if ui.configbool("rage", "fastmanifestcached", False):
        detailed.append(
            ('hg sl -r "fastmanifestcached()"',
                _failsafe(lambda: hgcmd('smartlog',
                          rev=["fastmanifestcached()"]))),
        )

    msg = '\n'.join(map(format, basic)) + '\n' +\
          '\n'.join(map(lambda x: format(x, False), detailed))
    if _failsafeerrors:
        msg += '\n' + '\n'.join(_failsafeerrors)

    ui._colormode = oldcolormode
    return msg

@command('^rage', rageopts, _('hg rage'))
def rage(ui, repo, *pats, **opts):
    """collect useful diagnostics for asking help from the source control team

    The rage command collects useful diagnostic information.

    By default, the information will be uploaded to Phabricator and
    instructions about how to ask for help will be printed.

    After submitting to Phabricator, it prints configerable advice::

    [rage]
    advice = Please see our FAQ guide: https://...

    """
    with progress.spinner(ui, "Generating paste"):
        msg = _makerage(ui, repo, **opts)

    if opts.get('preview'):
        ui.write('%s\n' % msg)
        return

    with progress.spinner(ui, "Saving paste"):
        try:
            p = subprocess.Popen(
                ['arc', 'paste', '--lang', 'hgrage' , '--title', 'hgrage'],
                stdout=subprocess.PIPE,
                stdin=subprocess.PIPE,
                stderr=subprocess.PIPE)
            out, err = p.communicate(input=msg + '\n')
            ret = p.returncode
        except (WindowsError, OSError):
            ui.write(_('Failed calling arc. (is it in your PATH?)\n'))
            ret = 1

    if ret:
        fd, tmpname = tempfile.mkstemp(prefix='hg-rage-')
        with os.fdopen(fd, r'w') as tmpfp:
            tmpfp.write(msg)
            ui.write(_('Failed to post the diagnostic paste to Phabricator, '
                      'but its contents have been written to:\n\n'))
            ui.write(_('  %s\n') % tmpname, label='rage.link')
            ui.write(_('\nPlease include this file in the %s.\n') %
                     ui.config('ui', 'supportcontact'))
    else:
        ui.write(_('Please post in %s with the following link:\n\n')
                 % (ui.config('ui', 'supportcontact')))
        ui.write('  ' + out + '\n', label='rage.link')
    ui.write(ui.config('rage', 'advice', '') + '\n')

colortable = {
    'rage.link': 'blue bold',
}
