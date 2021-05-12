/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use megarepo_config::Target;
use scuba_ext::MononokeScubaSampleBuilder;

pub(crate) fn hex(v: &[u8]) -> String {
    faster_hex::hex_string(v).expect("hex_string should never fail")
}

pub(crate) enum Reported {
    Param,
    Response,
}

pub(crate) fn report_megarepo_target(
    target: &Target,
    scuba: &mut MononokeScubaSampleBuilder,
    reported: Reported,
) {
    match reported {
        Reported::Param => {
            scuba.add("param_megarepo_target_bookmark", target.bookmark.clone());
            scuba.add("param_megarepo_target_repo_id", target.repo_id);
        }
        Reported::Response => {
            scuba.add("response_megarepo_target_bookmark", target.bookmark.clone());
            scuba.add("response_megarepo_target_repo_id", target.repo_id);
        }
    }
}
