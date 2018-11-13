  $ . $TESTDIR/library.sh

setup configuration
  $ setup_hg_config_repo
  $ cd "$TESTTMP/mononoke-config"

  $ cat >> repos/repo/server.toml <<CONFIG
  > [[bookmarks]]
  > name="master_bookmark"
  > CONFIG

  $ mkdir -p common/hooks
  $ cat > common/hooks/file_size_hook.lua <<CONFIG
  > hook = function (ctx)
  >  if ctx.file.len() > 10 then
  >    return false, "File is too large"
  >  end
  >  return true
  > end
  > CONFIG
  $ register_hook file_size_hook common/hooks/file_size_hook.lua PerAddedOrModifiedFile "bypass_commit_string=\"@allow_large_files\""

  $ cat > common/hooks/no_owners_file_deletes.lua <<CONFIG
  > hook = function (ctx)
  >   for _, f in ipairs(ctx.files) do
  >     if f.is_deleted() and string.match(f.path, ".*OWNERS$") then
  >       return false, "Deletion of OWNERS files is not allowed"
  >     end
  >   end
  >   return true
  > end
  > CONFIG
  $ register_hook no_owners_file_deletes common/hooks/no_owners_file_deletes.lua PerChangeset "bypass_commit_string=\"@allow_delete_owners\""

  $ cat > common/hooks/no_owners2_file_deletes_pushvars.lua <<CONFIG
  > hook = function (ctx)
  >   for _, f in ipairs(ctx.files) do
  >     if f.is_deleted() and string.match(f.path, ".*OWNERS2$") then
  >       return false, "Deletion of OWNERS files is not allowed"
  >     end
  >   end
  >   return true
  > end
  > CONFIG
  $ register_hook no_owners2_file_deletes_pushvars common/hooks/no_owners2_file_deletes_pushvars.lua PerChangeset "bypass_pushvar=\"ALLOW_DELETE_OWNERS=true\""

  $ commit_and_blobimport_config_repo
  $ setup_common_hg_configs
  $ cd $TESTTMP


setup common configuration
  $ cat >> $HGRCPATH <<EOF
  > [ui]
  > ssh="$DUMMYSSH"
  > EOF

setup repo
  $ hg init repo-hg
  $ cd repo-hg
  $ setup_hg_server
  $ hg debugdrawdag <<EOF
  > C
  > |
  > B
  > |
  > A
  > EOF

create master bookmark

  $ hg bookmark master_bookmark -r tip

blobimport them into Mononoke storage and start Mononoke
  $ cd ..
  $ blobimport rocksdb repo-hg/.hg repo

start mononoke
  $ mononoke
  $ wait_for_mononoke $TESTTMP/repo

Clone the repo
  $ hgclone_treemanifest ssh://user@dummy/repo-hg repo2 --noupdate --config extensions.remotenames= -q
  $ cd repo2
  $ setup_hg_client
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > pushrebase =
  > remotenames =
  > EOF

  $ hg up -q 0
  $ echo 1 > 1 && hg add 1 && hg ci -m 1
  $ hgmn push -r . --to master_bookmark -q
  server ignored bookmark master_bookmark update

Delete a file, make sure that file_size_hook is not called on deleted files
  $ hgmn up -q tip
  $ hg rm 1
  $ hg ci -m 'delete a file'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 8ecfb5e6aa64 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 0 files
  server ignored bookmark master_bookmark update

Add OWNERS file, then delete it. Make sure deletion is not allowed
  $ touch OWNERS && hg add OWNERS && hg ci -m 'add OWNERS'
  $ hgmn push -r . --to master_bookmark -q
  server ignored bookmark master_bookmark update
  $ hg rm OWNERS
  $ hg ci -m 'remove OWNERS'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 2d1a0bcf73ee to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: * ERRO Command failed, remote: true, error: hooks failed: (glob)
  remote: no_owners_file_deletes for 2d1a0bcf73ee48cde9073fd52b6bbb71e4459c9b: Deletion of OWNERS files is not allowed, root_cause: ErrorMessage {
  remote:     msg: "hooks failed:\nno_owners_file_deletes for 2d1a0bcf73ee48cde9073fd52b6bbb71e4459c9b: Deletion of OWNERS files is not allowed"
  remote: }, backtrace: , session_uuid: * (glob)
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]

Bypass owners check
  $ cat >> .hg/hgrc <<CONFIG
  > [extensions]
  > amend=
  > CONFIG
  $ hg amend -m 'remove OWNERS\n@allow_delete_owners'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 67730b0d6122 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 0 files
  server ignored bookmark master_bookmark update

Add OWNERS2 file. This time bypass it with pushvars
  $ touch OWNERS2 && hg ci -Aqm 'add OWNERS2'
  $ hgmn push -r . --to master_bookmark -q
  server ignored bookmark master_bookmark update
  $ hg rm OWNERS2
  $ hg ci -m 'remove OWNERS2'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 55334cb4e1e4 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: * ERRO Command failed, remote: true, error: hooks failed: (glob)
  remote: no_owners2_file_deletes_pushvars for 55334cb4e1e487f6de665629326eb1aaddccde53: Deletion of OWNERS files is not allowed, root_cause: ErrorMessage {
  remote:     msg: "hooks failed:\nno_owners2_file_deletes_pushvars for 55334cb4e1e487f6de665629326eb1aaddccde53: Deletion of OWNERS files is not allowed"
  remote: }, backtrace: , session_uuid: * (glob)
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]
  $ hgmn push -r . --to master_bookmark --pushvars "ALLOW_DELETE_OWNERS=true"
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 55334cb4e1e4 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 0 files
  server ignored bookmark master_bookmark update

Send large file
  $ hg up -q 0
  $ echo 'aaaaaaaaaaa' > largefile
  $ hg ci -Aqm 'largefile'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 3e0db158edcc to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: * ERRO Command failed, remote: true, error: hooks failed: (glob)
  remote: file_size_hook for 3e0db158edcc82d93b971f44c13ac74836db5714: File is too large, root_cause: ErrorMessage {
  remote:     msg: "hooks failed:\nfile_size_hook for 3e0db158edcc82d93b971f44c13ac74836db5714: File is too large"
  remote: }, backtrace: , session_uuid: * (glob)
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]

Bypass large file hook
  $ hg amend -m '@allow_large_files'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev 51fea0e7527d to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 0 changes to 0 files
  server ignored bookmark master_bookmark update

Send large file inside a directory
  $ hg up -q 0
  $ mkdir dir/
  $ echo 'aaaaaaaaaaa' > dir/largefile
  $ hg ci -Aqm 'dir/largefile'
  $ hgmn push -r . --to master_bookmark
  remote: * DEBG Session with Mononoke started with uuid: * (glob)
  pushing rev cbc62a724366 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: * ERRO Command failed, remote: true, error: hooks failed: (glob)
  remote: file_size_hook for cbc62a724366fbea4663ca3e1f1a834af9f2f992: File is too large, root_cause: ErrorMessage {
  remote:     msg: "hooks failed:\nfile_size_hook for cbc62a724366fbea4663ca3e1f1a834af9f2f992: File is too large"
  remote: }, backtrace: , session_uuid: * (glob)
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]
