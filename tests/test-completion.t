#require no-fsmonitor

Show all commands except debug commands
  $ hg debugcomplete
  add
  addremove
  annotate
  archive
  backout
  bisect
  bookmarks
  branch
  branches
  bundle
  cat
  clone
  commit
  config
  copy
  diff
  export
  files
  forget
  githelp
  graft
  grep
  heads
  help
  hint
  histgrep
  identify
  import
  incoming
  init
  locate
  log
  manifest
  merge
  outgoing
  parents
  paths
  phase
  pull
  push
  record
  recover
  remove
  rename
  resolve
  revert
  rollback
  root
  serve
  show
  status
  summary
  tag
  tags
  tip
  unbundle
  update
  verify
  version

Show all commands that start with "a"
  $ hg debugcomplete a
  add
  addremove
  annotate
  archive

Do not show debug commands if there are other candidates
  $ hg debugcomplete d
  diff

Show debug commands if there are no other candidates
  $ hg debugcomplete debug
  debugancestor
  debugapplystreamclonebundle
  debugbuilddag
  debugbundle
  debugcapabilities
  debugcheckcasecollisions
  debugcheckstate
  debugcolor
  debugcommands
  debugcomplete
  debugconfig
  debugcreatestreamclonebundle
  debugdag
  debugdata
  debugdate
  debugdeltachain
  debugdirstate
  debugdiscovery
  debugdrawdag
  debugextensions
  debugfilerevision
  debugfileset
  debugformat
  debugfsinfo
  debuggetbundle
  debugignore
  debugindex
  debugindexdot
  debuginstall
  debugknown
  debuglabelcomplete
  debuglocks
  debugmergestate
  debugnamecomplete
  debugobsolete
  debugpathcomplete
  debugpickmergetool
  debugprocesstree
  debugprogress
  debugpushkey
  debugpvec
  debugrebuilddirstate
  debugrebuildfncache
  debugrename
  debugrevlog
  debugrevspec
  debugsetparents
  debugshell
  debugssl
  debugsub
  debugsuccessorssets
  debugtemplate
  debugtreestate
  debugupdatecaches
  debugupgraderepo
  debugwalk
  debugwireargs

Do not show the alias of a debug command if there are other candidates
(this should hide rawcommit)
  $ hg debugcomplete r
  record
  recover
  remove
  rename
  resolve
  revert
  rollback
  root
Show the alias of a debug command if there are no other candidates
  $ hg debugcomplete rawc
  

Show the global options
  $ hg debugcomplete --options | sort
  --color
  --config
  --configfile
  --cwd
  --debug
  --debugger
  --encoding
  --encodingmode
  --help
  --hidden
  --noninteractive
  --pager
  --profile
  --quiet
  --repository
  --time
  --traceback
  --verbose
  --version
  -R
  -h
  -q
  -v
  -y

Show the options for the "serve" command
  $ hg debugcomplete --options serve | sort
  --accesslog
  --address
  --certificate
  --cmdserver
  --color
  --config
  --configfile
  --cwd
  --daemon
  --daemon-postexec
  --debug
  --debugger
  --encoding
  --encodingmode
  --errorlog
  --help
  --hidden
  --ipv6
  --name
  --noninteractive
  --pager
  --pid-file
  --port
  --port-file
  --prefix
  --profile
  --quiet
  --read-only
  --repository
  --stdio
  --style
  --subrepos
  --templates
  --time
  --traceback
  --verbose
  --version
  --web-conf
  -6
  -A
  -E
  -R
  -S
  -a
  -d
  -h
  -n
  -p
  -q
  -t
  -v
  -y

Show an error if we use --options with an ambiguous abbreviation
  $ hg debugcomplete --options s
  hg: command 's' is ambiguous:
      serve show showconfig status summary
  [255]

Show all commands + options
  $ hg debugcommands
  add: include, exclude, subrepos, dry-run
  annotate: rev, follow, no-follow, text, user, file, date, number, changeset, line-number, skip, ignore-all-space, ignore-space-change, ignore-blank-lines, ignore-space-at-eol, include, exclude, template
  clone: noupdate, updaterev, rev, branch, pull, uncompressed, stream, ssh, remotecmd, insecure
  commit: addremove, close-branch, amend, secret, edit, interactive, reuse-message, include, exclude, message, logfile, date, user, subrepos
  diff: rev, change, text, git, binary, nodates, noprefix, show-function, reverse, ignore-all-space, ignore-space-change, ignore-blank-lines, ignore-space-at-eol, unified, stat, root, only-files-in-revs, include, exclude, subrepos
  export: output, switch-parent, rev, text, git, binary, nodates
  forget: include, exclude
  githelp: 
  init: ssh, remotecmd, insecure
  log: follow, follow-first, date, copies, keyword, rev, line-range, removed, only-merges, user, only-branch, branch, prune, patch, git, limit, no-merges, stat, graph, style, template, include, exclude
  merge: force, rev, preview, tool
  pull: update, force, rev, bookmark, branch, ssh, remotecmd, insecure
  push: force, rev, bookmark, branch, new-branch, pushvars, ssh, remotecmd, insecure
  record: addremove, close-branch, amend, secret, edit, message, logfile, date, user, ignore-all-space, ignore-space-change, ignore-blank-lines, ignore-space-at-eol, subrepos, include, exclude
  remove: after, force, subrepos, include, exclude
  serve: accesslog, daemon, daemon-postexec, errorlog, port, address, prefix, name, web-conf, webdir-conf, pid-file, port-file, stdio, cmdserver, templates, style, ipv6, certificate, read-only, subrepos
  show: nodates, noprefix, stat, git, unified, ignore-all-space, ignore-space-change, ignore-blank-lines, ignore-space-at-eol, style, template, include, exclude
  status: all, modified, added, removed, deleted, clean, unknown, ignored, no-status, terse, copies, print0, rev, change, include, exclude, subrepos, template
  summary: remote
  update: clean, check, merge, date, rev, inactive, tool
  addremove: similarity, subrepos, include, exclude, dry-run
  archive: no-decode, prefix, rev, type, subrepos, include, exclude
  backout: merge, commit, no-commit, parent, rev, edit, tool, include, exclude, message, logfile, date, user
  bisect: reset, good, bad, skip, extend, command, noupdate, nosparseskip
  bookmarks: force, rev, delete, strip, rename, inactive, template
  branch: force, clean, new
  branches: active, closed, template
  bundle: force, rev, branch, base, all, type, ssh, remotecmd, insecure
  cat: output, rev, decode, include, exclude, template
  config: untrusted, edit, local, global, template
  copy: after, force, include, exclude, dry-run
  debugancestor: 
  debugapplystreamclonebundle: 
  debugbuilddag: mergeable-file, overwritten-file, new-file
  debugbundle: all, part-type, spec
  debugcapabilities: 
  debugcheckcasecollisions: rev
  debugcheckstate: 
  debugcolor: style
  debugcommands: 
  debugcomplete: options
  debugcreatestreamclonebundle: 
  debugdag: tags, branches, dots, spaces
  debugdata: changelog, manifest, dir
  debugdate: extended
  debugdeltachain: changelog, manifest, dir, template
  debugdirstate: nodates, datesort
  debugdiscovery: old, nonheads, rev, ssh, remotecmd, insecure
  debugdrawdag: 
  debugextensions: excludedefault, template
  debugfilerevision: rev, include, exclude
  debugfileset: rev
  debugformat: template
  debugfsinfo: 
  debuggetbundle: head, common, type
  debugignore: 
  debugindex: changelog, manifest, dir, format
  debugindexdot: changelog, manifest, dir
  debuginstall: template
  debugknown: 
  debuglabelcomplete: 
  debuglocks: force-lock, force-wlock, force-undolog-lock, set-lock, set-wlock
  debugmergestate: 
  debugnamecomplete: 
  debugobsolete: flags, record-parents, rev, exclusive, index, delete, date, user, template
  debugpathcomplete: full, normal, added, removed
  debugpickmergetool: rev, changedelete, include, exclude, tool
  debugprocesstree: 
  debugprogress: spinner, nototal, bytes, with-output
  debugpushkey: 
  debugpvec: 
  debugrebuilddirstate: rev, minimal
  debugrebuildfncache: 
  debugrename: rev
  debugrevlog: changelog, manifest, dir, dump
  debugrevspec: optimize, show-revs, show-set, show-stage, no-optimized, verify-optimized
  debugsetparents: 
  debugshell: command
  debugssl: 
  debugsub: rev
  debugsuccessorssets: closest
  debugtemplate: rev, define
  debugtreestate: 
  debugupdatecaches: 
  debugupgraderepo: optimize, run
  debugwalk: include, exclude
  debugwireargs: three, four, five, ssh, remotecmd, insecure
  files: rev, print0, include, exclude, template, subrepos
  graft: rev, continue, abort, edit, log, force, currentdate, currentuser, date, user, tool, dry-run
  grep: after-context, before-context, context, ignore-case, files-with-matches, line-number, invert-match, word-regexp, extended-regexp, fixed-strings, perl-regexp, include, exclude
  heads: rev, topo, active, closed, style, template
  help: extension, command, keyword, system
  hint: ack
  histgrep: print0, all, text, follow, ignore-case, files-with-matches, line-number, rev, user, date, template, include, exclude
  identify: rev, num, id, branch, tags, bookmarks, ssh, remotecmd, insecure, template
  import: strip, base, edit, force, no-commit, bypass, partial, exact, prefix, import-branch, message, logfile, date, user, similarity
  incoming: force, newest-first, bundle, rev, bookmarks, branch, patch, git, limit, no-merges, stat, graph, style, template, ssh, remotecmd, insecure, subrepos
  locate: rev, print0, fullpath, include, exclude
  manifest: rev, all, template
  outgoing: force, rev, newest-first, bookmarks, branch, patch, git, limit, no-merges, stat, graph, style, template, ssh, remotecmd, insecure, subrepos
  parents: rev, style, template
  paths: template
  phase: public, draft, secret, force, rev
  recover: 
  rename: after, force, include, exclude, dry-run
  resolve: all, list, mark, unmark, no-status, root-relative, tool, include, exclude, template, skip
  revert: all, date, rev, no-backup, interactive, include, exclude, dry-run
  rollback: dry-run, force
  root: 
  tag: force, local, rev, remove, edit, message, date, user
  tags: template
  tip: patch, git, style, template
  unbundle: update
  verify: rev
  version: template

  $ hg init a
  $ cd a
  $ echo fee > fee
  $ hg ci -q -Amfee
  $ hg tag fee
  $ mkdir fie
  $ echo dead > fie/dead
  $ echo live > fie/live
  $ hg bookmark fo
  $ hg branch -q fie
  $ hg ci -q -Amfie
  $ echo fo > fo
  $ hg branch -qf default
  $ hg ci -q -Amfo
  $ echo Fum > Fum
  $ hg ci -q -AmFum
  $ hg bookmark Fum

Test debugpathcomplete

  $ hg debugpathcomplete f
  fee
  fie
  fo
  $ hg debugpathcomplete -f f
  fee
  fie/dead
  fie/live
  fo

  $ hg rm Fum
  $ hg debugpathcomplete -r F
  Fum

Test debugnamecomplete

  $ hg debugnamecomplete
  Fum
  default
  fee
  fie
  fo
  tip
  $ hg debugnamecomplete f
  fee
  fie
  fo

Test debuglabelcomplete, a deprecated name for debugnamecomplete that is still
used for completions in some shells.

  $ hg debuglabelcomplete
  Fum
  default
  fee
  fie
  fo
  tip
  $ hg debuglabelcomplete f
  fee
  fie
  fo
