# Copyright (c) Facebook, Inc. and its affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ . "${TEST_FIXTURES}/library.sh"
  $ DISABLE_HTTP_CONTROL_API=1 setup_common_config
  $ mononoke
  $ wait_for_mononoke

  $ sslcurl -X POST -fsS "https://localhost:$MONONOKE_SOCKET/control/drop_bookmarks_cache"
  curl: (22) The requested URL returned error: 403 Forbidden
  [22]
