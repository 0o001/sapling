/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/nfs/NfsRequestContext.h"
#include <folly/Utility.h>

namespace facebook::eden {

NfsRequestContext::NfsRequestContext(
    uint32_t xid,
    folly::StringPiece causeDetail,
    ProcessAccessLog& processAccessLog)
    : RequestContext(processAccessLog), xid_(xid), causeDetail_(causeDetail) {}
} // namespace facebook::eden
