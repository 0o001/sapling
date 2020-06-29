# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ . "${TEST_FIXTURES}/library.sh"

setup configuration
  $ setup_mononoke_config
  $ cd "$TESTTMP/mononoke-config"

  $ cat >> repos/repo/server.toml <<CONFIG
  > [[bookmarks]]
  > name="master_bookmark"
  > CONFIG

  $ register_hook limit_filesize <(
  >   cat <<CONF 
  > bypass_commit_string="@allow_large_files"
  > config_ints={filesizelimit=10}
  > CONF
  > )

  $ setup_common_hg_configs
  $ cd $TESTTMP


setup common configuration
  $ cat >> $HGRCPATH <<EOF
  > [ui]
  > ssh="$DUMMYSSH"
  > [extensions]
  > amend=
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
  $ blobimport repo-hg/.hg repo

start mononoke
  $ mononoke
  $ wait_for_mononoke

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

Delete a file, make sure that file_size_hook is not called on deleted files
  $ hgmn up -q tip
  $ hg rm 1
  $ hg ci -m 'delete a file'
  $ hgmn push -r . --to master_bookmark
  pushing rev 8ecfb5e6aa64 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 0 changesets with 0 changes to 0 files
  updating bookmark master_bookmark

Send large file
  $ hg up -q 0
  $ echo 'aaaaaaaaaaa' > largefile
  $ hg ci -Aqm 'largefile'
  $ hgmn push -r . --to master_bookmark
  pushing rev 3e0db158edcc to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: Command failed
  remote:   Error:
  remote:     hooks failed:
  remote:     limit_filesize for 3e0db158edcc82d93b971f44c13ac74836db5714: File size limit is 10 bytes. You tried to push file largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions.
  remote: 
  remote:   Root cause:
  remote:     hooks failed:
  remote:     limit_filesize for 3e0db158edcc82d93b971f44c13ac74836db5714: File size limit is 10 bytes. You tried to push file largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions.
  remote: 
  remote:   Debug context:
  remote:     "hooks failed:\nlimit_filesize for 3e0db158edcc82d93b971f44c13ac74836db5714: File size limit is 10 bytes. You tried to push file largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions."
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]

Bypass large file hook
  $ hg amend -m '@allow_large_files'
  $ hgmn push -r . --to master_bookmark
  pushing rev 51fea0e7527d to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  added 1 changesets with 0 changes to 0 files
  updating bookmark master_bookmark

Send large file inside a directory
  $ hg up -q 0
  $ mkdir dir/
  $ echo 'aaaaaaaaaaa' > dir/largefile
  $ hg ci -Aqm 'dir/largefile'
  $ hgmn push -r . --to master_bookmark
  pushing rev cbc62a724366 to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  remote: Command failed
  remote:   Error:
  remote:     hooks failed:
  remote:     limit_filesize for cbc62a724366fbea4663ca3e1f1a834af9f2f992: File size limit is 10 bytes. You tried to push file dir/largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions.
  remote: 
  remote:   Root cause:
  remote:     hooks failed:
  remote:     limit_filesize for cbc62a724366fbea4663ca3e1f1a834af9f2f992: File size limit is 10 bytes. You tried to push file dir/largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions.
  remote: 
  remote:   Debug context:
  remote:     "hooks failed:\nlimit_filesize for cbc62a724366fbea4663ca3e1f1a834af9f2f992: File size limit is 10 bytes. You tried to push file dir/largefile that is over the limit (12 bytes).  See https://fburl.com/landing_big_diffs for instructions."
  abort: stream ended unexpectedly (got 0 bytes, expected 4)
  [255]
