#chg-compatible

  $ configure modern

  $ setconfig paths.default=test:e1 ui.traceback=1
  $ export LOG=edenscm::mercurial::eagerpeer=trace,eagerepo=trace

Disable SSH:

  $ setconfig ui.ssh=false

Prepare Repo:

  $ newremoterepo
  $ setconfig paths.default=test:e1
  $ drawdag << 'EOS'
  >   D
  >   |
  > B C  # C/T/A=2
  > |/
  > A    # A/T/A=1
  > EOS

Push:

  $ hg push -r $C --to master --create
  pushing rev 178c10ffbc2f to destination test:e1 bookmark master
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict()
   DEBUG eagerepo::api: commit_known 178c10ffbc2f92d5407c14478ae9d9dea81f232e
   TRACE edenscm::mercurial::eagerpeer: known 178c10ffbc2f92d5407c14478ae9d9dea81f232e: False
   DEBUG edenscm::mercurial::eagerpeer: heads = []
  searching for changes
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict()
   TRACE edenscm::mercurial::eagerpeer: adding   blob 005d992c5dcf32993668f7cede29d296c494a5d9
   TRACE edenscm::mercurial::eagerpeer: adding   blob f976da1d0df2256cde08db84261621d5e92f77be
   TRACE edenscm::mercurial::eagerpeer: adding   tree 4c28a8a0e46c55df521ea9d682b5b6b8a91031a2
   TRACE edenscm::mercurial::eagerpeer: adding   tree 6161efd5db4f6d976d6aba647fa77c12186d3179
   TRACE edenscm::mercurial::eagerpeer: adding commit 748104bd5058bf2c386d074d8dcf2704855380f6
   TRACE edenscm::mercurial::eagerpeer: adding   blob a2e456504a5e61f763f1a0b36a6c247c7541b2b3
   TRACE edenscm::mercurial::eagerpeer: adding   blob d85e50a0f00eee8211502158e93772aec5dc3d63
   TRACE edenscm::mercurial::eagerpeer: adding   tree 319bc9670b2bff0a75b8b2dfa78867bf1f8d7aec
   TRACE edenscm::mercurial::eagerpeer: adding   tree 0ccf968573574750913fcee533939cc7ebe7327d
   TRACE edenscm::mercurial::eagerpeer: adding commit 178c10ffbc2f92d5407c14478ae9d9dea81f232e
   DEBUG edenscm::mercurial::eagerpeer: flushed
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict()
   DEBUG edenscm::mercurial::eagerpeer: flushed
   DEBUG edenscm::mercurial::eagerpeer: pushkey bookmarks 'master': '' => '178c10ffbc2f92d5407c14478ae9d9dea81f232e' (success)
  exporting bookmark master
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])

  $ hg push -r $B --allow-anon
  pushing to test:e1
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])
   DEBUG eagerepo::api: commit_known 178c10ffbc2f92d5407c14478ae9d9dea81f232e, 99dac869f01e09fe3d501fa645ea524af80d498f
   TRACE edenscm::mercurial::eagerpeer: known 178c10ffbc2f92d5407c14478ae9d9dea81f232e: True
   TRACE edenscm::mercurial::eagerpeer: known 99dac869f01e09fe3d501fa645ea524af80d498f: False
  searching for changes
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])
   TRACE edenscm::mercurial::eagerpeer: adding   blob 35e7525ce3a48913275d7061dd9a867ffef1e34d
   TRACE edenscm::mercurial::eagerpeer: adding   tree d8dc55ad2b89cdc0f1ee969e5d79bd1eaddb5b43
   TRACE edenscm::mercurial::eagerpeer: adding commit 99dac869f01e09fe3d501fa645ea524af80d498f
   DEBUG edenscm::mercurial::eagerpeer: flushed
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])

  $ hg push -r $D --to master
  pushing rev 23d30dc6b703 to destination test:e1 bookmark master
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])
   DEBUG eagerepo::api: commit_known 178c10ffbc2f92d5407c14478ae9d9dea81f232e, 23d30dc6b70380b2d939023947578ae0e0198999
   TRACE edenscm::mercurial::eagerpeer: known 178c10ffbc2f92d5407c14478ae9d9dea81f232e: True
   TRACE edenscm::mercurial::eagerpeer: known 23d30dc6b70380b2d939023947578ae0e0198999: False
  searching for changes
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])
   TRACE edenscm::mercurial::eagerpeer: adding   blob 4eec8cfdabce9565739489483b6ad93ef7657ea9
   TRACE edenscm::mercurial::eagerpeer: adding   tree 4a38281d93dab71e695b39f85bdfbac0ce78011d
   TRACE edenscm::mercurial::eagerpeer: adding commit 23d30dc6b70380b2d939023947578ae0e0198999
   DEBUG edenscm::mercurial::eagerpeer: flushed
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '178c10ffbc2f92d5407c14478ae9d9dea81f232e')])
   DEBUG edenscm::mercurial::eagerpeer: flushed
   DEBUG edenscm::mercurial::eagerpeer: pushkey bookmarks 'master': '178c10ffbc2f92d5407c14478ae9d9dea81f232e' => '23d30dc6b70380b2d939023947578ae0e0198999' (success)
  updating bookmark master
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])

Pull:

  $ newremoterepo
  $ setconfig paths.default=test:e1
  $ hg debugchangelog --migrate lazy
  $ hg pull -B master
  pulling from test:e1
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: commit_known 
   DEBUG eagerepo::api: commit_graph 23d30dc6b70380b2d939023947578ae0e0198999 
   TRACE edenscm::mercurial::eagerpeer: graph node 748104bd5058bf2c386d074d8dcf2704855380f6 []
   TRACE edenscm::mercurial::eagerpeer: graph node 178c10ffbc2f92d5407c14478ae9d9dea81f232e ['748104bd5058bf2c386d074d8dcf2704855380f6']
   TRACE edenscm::mercurial::eagerpeer: graph node 23d30dc6b70380b2d939023947578ae0e0198999 ['178c10ffbc2f92d5407c14478ae9d9dea81f232e']

  $ hg pull -r $B
  pulling from test:e1
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: commit_known 99dac869f01e09fe3d501fa645ea524af80d498f
   TRACE edenscm::mercurial::eagerpeer: known 99dac869f01e09fe3d501fa645ea524af80d498f: True
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])
   DEBUG eagerepo::api: commit_known 23d30dc6b70380b2d939023947578ae0e0198999
   TRACE edenscm::mercurial::eagerpeer: known 23d30dc6b70380b2d939023947578ae0e0198999: True
  searching for changes
   DEBUG eagerepo::api: commit_graph 99dac869f01e09fe3d501fa645ea524af80d498f 23d30dc6b70380b2d939023947578ae0e0198999
   TRACE edenscm::mercurial::eagerpeer: graph node 99dac869f01e09fe3d501fa645ea524af80d498f ['748104bd5058bf2c386d074d8dcf2704855380f6']

  $ hg log -Gr 'all()' -T '{desc} {remotenames}'
   DEBUG eagerepo::api: revlog_data 99dac869f01e09fe3d501fa645ea524af80d498f, 23d30dc6b70380b2d939023947578ae0e0198999, 178c10ffbc2f92d5407c14478ae9d9dea81f232e, 748104bd5058bf2c386d074d8dcf2704855380f6
   TRACE eagerepo::api:  found: 99dac869f01e09fe3d501fa645ea524af80d498f, 94 bytes
   TRACE eagerepo::api:  found: 23d30dc6b70380b2d939023947578ae0e0198999, 94 bytes
   TRACE eagerepo::api:  found: 178c10ffbc2f92d5407c14478ae9d9dea81f232e, 98 bytes
   TRACE eagerepo::api:  found: 748104bd5058bf2c386d074d8dcf2704855380f6, 98 bytes
  o  B
  │
  │ o  D remote/master
  │ │
  │ o  C
  ├─╯
  o  A
  
Trigger file and tree downloading:

  $ hg cat -r $B B A
   DEBUG eagerepo::api: trees d8dc55ad2b89cdc0f1ee969e5d79bd1eaddb5b43
   TRACE eagerepo::api:  found: d8dc55ad2b89cdc0f1ee969e5d79bd1eaddb5b43, 170 bytes
   DEBUG eagerepo::api: files 005d992c5dcf32993668f7cede29d296c494a5d9
   TRACE eagerepo::api:  found: 005d992c5dcf32993668f7cede29d296c494a5d9, 41 bytes
   DEBUG eagerepo::api: files 35e7525ce3a48913275d7061dd9a867ffef1e34d
   TRACE eagerepo::api:  found: 35e7525ce3a48913275d7061dd9a867ffef1e34d, 41 bytes
  AB (no-eol)

Clone (using edenapi clonedata, bypassing peer interface):

  $ cd $TESTTMP
  $ hg clone -U --shallow test:e1 --config remotefilelog.reponame=x --config clone.force-edenapi-clonedata=1 cloned1
  fetching lazy changelog
   DEBUG eagerepo::api: clone_data
  populating main commit graph
  tip commit: 23d30dc6b70380b2d939023947578ae0e0198999
  fetching selected remote bookmarks
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])

Clone:

  $ cd $TESTTMP
  $ hg clone -U --shallow test:e1 cloned
   DEBUG eagerepo::api: clone_data
  populating main commit graph
  tip commit: 23d30dc6b70380b2d939023947578ae0e0198999
  fetching selected remote bookmarks
   DEBUG eagerepo::api: bookmarks master
   DEBUG edenscm::mercurial::eagerpeer: listkeyspatterns(bookmarks, ['master']) = sortdict([('master', '23d30dc6b70380b2d939023947578ae0e0198999')])

  $ cd cloned

Commit hash and message are lazy

  $ LOG=dag::protocol=debug,eagerepo=debug hg log -T '{desc} {node}\n' -r 'all()'
   DEBUG dag::protocol: resolve ids [0] remotely
   DEBUG eagerepo::api: revlog_data 748104bd5058bf2c386d074d8dcf2704855380f6, 178c10ffbc2f92d5407c14478ae9d9dea81f232e, 23d30dc6b70380b2d939023947578ae0e0198999
  A 748104bd5058bf2c386d074d8dcf2704855380f6
  C 178c10ffbc2f92d5407c14478ae9d9dea81f232e
  D 23d30dc6b70380b2d939023947578ae0e0198999

Read file content:

  $ hg cat -r $C C
   DEBUG eagerepo::api: trees 0ccf968573574750913fcee533939cc7ebe7327d
   TRACE eagerepo::api:  found: 0ccf968573574750913fcee533939cc7ebe7327d, 170 bytes
   DEBUG eagerepo::api: files a2e456504a5e61f763f1a0b36a6c247c7541b2b3
   TRACE eagerepo::api:  found: a2e456504a5e61f763f1a0b36a6c247c7541b2b3, 41 bytes
  C (no-eol)

Making a commit and amend:
(Triggers remote lookup 1 time!)

#if no-windows
(Path separator is different on Windows)

  $ echo Z > Z
  $ LOG=dag::protocol=debug,dag::open=debug,dag::cache=trace hg commit -Am Z Z
   DEBUG dag::open: open at "$TESTTMP/e1/.hg/store/segments/v1"
   DEBUG dag::open: open at "$TESTTMP/cloned/.hg/store/segments/v1"
   DEBUG dag::open: open at "$TESTTMP/e1/.hg/store/segments/v1"
   DEBUG dag::protocol: resolve names [567c5fc544ed12bf9619197fdd5263d6c3129cd0] remotely
   TRACE dag::cache: cached missing 567c5fc544ed12bf9619197fdd5263d6c3129cd0 (server confirmed)
   DEBUG dag::open: open at "$TESTTMP/cloned/.hg/store/segments/v1"
   DEBUG dag::cache: reusing cache (1 missing)

  $ LOG=dag::protocol=debug,dag::open=debug,dag::cache=trace hg amend -m Z1
   DEBUG dag::open: open at "$TESTTMP/e1/.hg/store/segments/v1"
   DEBUG dag::open: open at "$TESTTMP/cloned/.hg/store/segments/v1"
   DEBUG dag::open: open at "$TESTTMP/e1/.hg/store/segments/v1"
   DEBUG dag::protocol: resolve names [26ef60562bd4f4205f24250ea9d2e24e61108072] remotely
   TRACE dag::cache: cached missing 26ef60562bd4f4205f24250ea9d2e24e61108072 (server confirmed)
   DEBUG dag::open: open at "$TESTTMP/cloned/.hg/store/segments/v1"
   DEBUG dag::cache: reusing cache (1 missing)
   DEBUG dag::open: open at "$TESTTMP/cloned/.hg/store/segments/v1"
   DEBUG dag::cache: reusing cache (1 missing)

#endif
