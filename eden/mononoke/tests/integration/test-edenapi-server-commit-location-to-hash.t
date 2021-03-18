# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ . "${TEST_FIXTURES}/library.sh"

Set up local hgrc and Mononoke config.
  $ setup_common_config
  $ setup_configerator_configs
  $ cd $TESTTMP

Initialize test repo.
  $ hginit_treemanifest repo-hg
  $ cd repo-hg
  $ setup_hg_server

Populate test repo
  $ echo "my commit message" > test.txt
  $ hg commit -Aqm "add test.txt"
  $ COMMIT_1=$(hg log -r . -T '{node}')
  $ hg cp test.txt copy.txt
  $ hg commit -Aqm "copy test.txt to test2.txt"
  $ COMMIT_2=$(hg log -r . -T '{node}')
  $ echo "this is the second file" > test2.txt
  $ hg commit -Aqm "update test2.txt"
  $ COMMIT_B1=$(hg log -r . -T '{node}')
  $ hg co -q $COMMIT_2
  $ echo "this is the first file" > test.txt
  $ hg commit -Aqm "update test.txt"
  $ COMMIT_B2=$(hg log -r . -T '{node}')
  $ hg merge -q $COMMIT_B1
  $ hg commit -m "merge commit!!!"
  $ COMMIT_MERGE=$(hg log -r . -T '{node}')
  $ echo "third file" > test3.txt
  $ hg commit -Aqm "add test3.txt"
  $ COMMIT_M1=$(hg log -r . -T '{node}')
  $ hg bookmark "master_bookmark"
  $ hg log -G -T '{node} {desc}\n' -r "all()"
  @  b5bc5249412595662f15a1aca5ae50fec4a93628 add test3.txt
  │
  o    ce33edd793793f108fbe78aa90f3fedbeae09082 merge commit!!!
  ├─╮
  │ o  b6f0fa5a73b54553c0d4b6f483c8ef18efb3bde2 update test.txt
  │ │
  o │  45a08a9d95ee1053cf34273c8a427973d4ffd11a update test2.txt
  ├─╯
  o  c7dcf24fab3a8ab956273fa40d5cc44bc26ec655 copy test.txt to test2.txt
  │
  o  e83645968c8f2954b97a3c79ce5a6b90a464c54d add test.txt
  


Blobimport test repo.
  $ cd ..
  $ blobimport repo-hg/.hg repo

Start up EdenAPI server.
  $ SEGMENTED_CHANGELOG_ENABLE=1 setup_mononoke_config
  $ mononoke
  $ wait_for_mononoke

Create and send file data request.
  $ edenapi_make_req commit-location-to-hash > req.cbor <<EOF
  > {
  >   "requests": [{
  >       "location": {
  >           "descendant": "$COMMIT_B1",
  >           "distance": 1
  >       },
  >       "count": 2
  >     }, {
  >       "location": {
  >           "descendant": "$COMMIT_B1",
  >           "distance": 2
  >       },
  >       "count": 1
  >     }, {
  >       "location": {
  >           "descendant": "$COMMIT_M1",
  >           "distance": 1
  >       },
  >       "count": 1
  >     }
  >   ]
  > }
  > EOF
  Reading from stdin
  Generated request: WireCommitLocationToHashRequestBatch {
      requests: [
          WireCommitLocationToHashRequest {
              location: WireCommitLocation {
                  descendant: WireHgId("45a08a9d95ee1053cf34273c8a427973d4ffd11a"),
                  distance: 1,
              },
              count: 2,
          },
          WireCommitLocationToHashRequest {
              location: WireCommitLocation {
                  descendant: WireHgId("45a08a9d95ee1053cf34273c8a427973d4ffd11a"),
                  distance: 2,
              },
              count: 1,
          },
          WireCommitLocationToHashRequest {
              location: WireCommitLocation {
                  descendant: WireHgId("b5bc5249412595662f15a1aca5ae50fec4a93628"),
                  distance: 1,
              },
              count: 1,
          },
      ],
  }

  $ sslcurl -s "https://localhost:$MONONOKE_SOCKET/edenapi/repo/commit/location_to_hash" --data-binary @req.cbor > res.cbor

Check files in response.
  $ edenapi_read_res commit-location-to-hash res.cbor
  Reading from file: "res.cbor"
  LocationToHashRequest(known=45a08a9d95ee1053cf34273c8a427973d4ffd11a, dist=1, count=2)
    c7dcf24fab3a8ab956273fa40d5cc44bc26ec655
    e83645968c8f2954b97a3c79ce5a6b90a464c54d
  LocationToHashRequest(known=45a08a9d95ee1053cf34273c8a427973d4ffd11a, dist=2, count=1)
    e83645968c8f2954b97a3c79ce5a6b90a464c54d
  LocationToHashRequest(known=b5bc5249412595662f15a1aca5ae50fec4a93628, dist=1, count=1)
    ce33edd793793f108fbe78aa90f3fedbeae09082
