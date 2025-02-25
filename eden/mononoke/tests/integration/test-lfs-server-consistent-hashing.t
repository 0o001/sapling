# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ . "${TEST_FIXTURES}/library.sh"

# Create a repository
  $ setup_common_config
  $ REPOID=1 FILESTORE=1 FILESTORE_CHUNK_SIZE=10 setup_mononoke_repo_config repo1
  $ LIVE_CONFIG="${LOCAL_CONFIGERATOR_PATH}/live.json"
  $ cat > "$LIVE_CONFIG" << EOF
  > {
  >   "track_bytes_sent": false,
  >   "enable_consistent_routing": false,
  >   "disable_hostname_logging": false,
  >   "acl_check": false,
  >   "enforce_acl_check": false
  > }
  > EOF

# Start a LFS server, without an upstream
  $ LFS_LOG="$TESTTMP/lfs.log"
  $ LFS_ROOT="$(lfs_server --log "$LFS_LOG" --live-config "$(get_configerator_relative_path "${LIVE_CONFIG}")")"
  $ LFS_URI="$LFS_ROOT/repo1"

# Upload a blob
  $ yes A 2>/dev/null | head -c 2KiB | hg --config extensions.lfs= debuglfssend "$LFS_URI"
  ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746 2048

# Prepare a batch request for our new object
  $ cat > "batch.json" << EOF
  > {
  >     "operation": "download",
  >     "transfers": ["basic"],
  >     "objects": [
  >         {
  >             "oid": "ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746",
  >             "size": 2048
  >         }
  >     ]
  > }
  > EOF

# Make sure we get a normal download URL
  $ curl -s --data-binary @batch.json "$LFS_URI/objects/batch" | jq ".objects[0].actions.download.href"
  "http://$LOCALIP:*/repo1/download/d28548bc21aabf04d143886d717d72375e3deecd0dafb3d110676b70a192cb5d?server_hostname=*" (glob)

# Update the config to enable consistent routing
  $ sed -i 's/"enable_consistent_routing": false/"enable_consistent_routing": true/g' "$LIVE_CONFIG"

# Wait for it to be updated
  $ sleep 1

# Make sure we get a normal download URL
  $ curl -s --data-binary @batch.json "$LFS_URI/objects/batch" | jq ".objects[0].actions.download.href"
  "http://$LOCALIP:*/repo1/download/d28548bc21aabf04d143886d717d72375e3deecd0dafb3d110676b70a192cb5d?routing=ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746?server_hostname=*" (glob)

# Make sure we can read it back
  $ hg --config extensions.lfs= debuglfsreceive ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746 2048 "$LFS_URI" | sha256sum
  ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746  -

# Verify that we used the consistent URL
  $ tail -n 2 "$LFS_LOG"
  IN  > GET /repo1/download/d28548bc21aabf04d143886d717d72375e3deecd0dafb3d110676b70a192cb5d?routing=ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746&server_hostname=* - (glob)
  OUT < GET /repo1/download/d28548bc21aabf04d143886d717d72375e3deecd0dafb3d110676b70a192cb5d?routing=ab02c2a1923c8eb11cb3ddab70320746d71d32ad63f255698dc67c3295757746&server_hostname=* 200 OK (glob)
