# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ export COMMIT_SCRIBE_CATEGORY=mononoke_commits
  $ export DRAFT_COMMIT_SCRIBE_CATEGORY=draft_mononoke_commits
  $ . "${TEST_FIXTURES}/library.sh"

setup configuration
  $ INFINITEPUSH_NAMESPACE_REGEX='^scratch/.+$' setup_common_config
  $ cd $TESTTMP

setup repo

  $ hginit_treemanifest repo-hg
  $ cd repo-hg
  $ echo "a file content" > a
  $ hg add a
  $ hg ci -ma

setup master bookmarks

  $ hg bookmark master_bookmark -r 'tip'

verify content
  $ hg log
  commit:      0e7ec5675652
  bookmark:    master_bookmark
  user:        test
  date:        Thu Jan 01 00:00:00 1970 +0000
  summary:     a
  

  $ cd $TESTTMP
  $ blobimport repo-hg/.hg repo

setup two repos: one will be used to push from, another will be used
to pull these pushed commits

  $ hgclone_treemanifest ssh://user@dummy/repo-hg repo2
  $ hgclone_treemanifest ssh://user@dummy/repo-hg repo3
  $ cd repo2
  $ hg pull ssh://user@dummy/repo-hg
  pulling from ssh://user@dummy/repo-hg
  searching for changes
  no changes found

start mononoke

  $ mononoke
  $ wait_for_mononoke

BEGIN Creation of new commits

create new commits in repo2 and check that they are seen as outgoing

  $ mkdir b_dir
  $ echo "new a file content" > a
  $ echo "b file content" > b_dir/b
  $ hg add b_dir/b
  $ hg ci -mb
  $ hgmn push -r . --to master_bookmark --create --config extensions.remotenames= --config extensions.pushrebase=
  pushing rev bb0985934a0f to destination ssh://user@dummy/repo bookmark master_bookmark
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  updating bookmark master_bookmark

  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .repo_name
  "repo"
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .bookmark
  "master_bookmark"
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .changeset_id
  "022352db2112d2f43ca2635686a6275ade50d612865551fa8d1f392b375e412e"
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .changed_files_count
  2
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .changed_files_size
  34
  $ rm "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY"

  $ echo forcepushrebase > forcepushrebase
  $ hg add -q forcepushrebase
  $ hg ci -m forcepushrebase
  $ hgmn push -r . --to forcepushrebase --create --force --config extensions.remotenames= --config extensions.pushrebase=
  pushing rev 0c1e5152244c to destination ssh://user@dummy/repo bookmark forcepushrebase
  searching for changes
  adding changesets
  adding manifests
  adding file changes
  exporting bookmark forcepushrebase
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .bookmark
  "forcepushrebase"
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .changeset_id
  "cf79ab3ba838b597ca4973ba397b4b687f54d9eed2f0edc4f950f3b80a68f8b3"


Use normal push (non-pushrebase)
  $ rm "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY"
  $ echo push > push
  $ hg add -q push
  $ hg ci -m 'commit'
  $ hgmn push --force
  pushing to ssh://user@dummy/repo
  searching for changes

  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .bookmark
  null
  $ cat "$TESTTMP/scribe_logs/$COMMIT_SCRIBE_CATEGORY" | jq .changeset_id
  "f76800ae3d688512180e7a0805ff18d39f7ea81617bce1aea4e11364584b007a"

Use infinitepush push
  $ cat >> .hg/hgrc <<EOF
  > [extensions]
  > commitcloud=
  > infinitepush=
  > remotenames=
  > [infinitepush]
  > server=False
  > branchpattern=re:^scratch/.+$
  > EOF
  $ hgmn up -q master_bookmark

Stop tracking master_bookmark
  $ hg up -q .
  $ echo pushbackup > pushbackup
  $ hg add -q pushbackup
  $ hg ci -m pushbackup
  $ hgmn pushbackup -r .
  backing up stack rooted at 0ed0fbff8a24
  commitcloud: backed up 1 commit
  $ cat "$TESTTMP/scribe_logs/$DRAFT_COMMIT_SCRIBE_CATEGORY" | jq .bookmark
  null
  $ cat "$TESTTMP/scribe_logs/$DRAFT_COMMIT_SCRIBE_CATEGORY" | jq .changeset_id
  "29259d73c8207a083a44f2635df387b194f76c41d2ccb71e7529ec0f70a4af28"
  $ rm "$TESTTMP/scribe_logs/$DRAFT_COMMIT_SCRIBE_CATEGORY"

  $ hgmn up -q master_bookmark
  $ echo infinitepush > infinitepush
  $ hg add -q infinitepush
  $ hg ci -m 'infinitepush'
  $ hgmn push ssh://user@dummy/repo -r . --to "scratch/123" --create
  pushing to ssh://user@dummy/repo
  searching for changes
  $ cat "$TESTTMP/scribe_logs/$DRAFT_COMMIT_SCRIBE_CATEGORY" | jq .bookmark
  "scratch/123"
  $ cat "$TESTTMP/scribe_logs/$DRAFT_COMMIT_SCRIBE_CATEGORY" | jq .changeset_id
  "06b8cee4d65704bcb81b988c1153daee3063d9e565f4d65e9e68475676b2438b"
