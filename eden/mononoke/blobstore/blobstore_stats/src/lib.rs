/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::convert::AsRef;
use std::time::Duration;

use anyhow::Error;
use futures_stats::FutureStats;
use scuba_ext::{MononokeScubaSampleBuilder, ScubaValue};
use time_ext::DurationExt;

use blobstore::{BlobstoreGetData, OverwriteStatus};
use context::PerfCounters;
use metaconfig_types::BlobstoreId;
use tunables::tunables;

const SLOW_REQUEST_THRESHOLD: Duration = Duration::from_secs(5);

const BLOBSTORE_ID: &str = "blobstore_id";
const COMPLETION_TIME: &str = "completion_time";
const ERROR: &str = "error";
const KEY: &str = "key";
const OPERATION: &str = "operation";
const SESSION: &str = "session";
const SIZE: &str = "size";
const WRITE_ORDER: &str = "write_order";

const OVERWRITE_STATUS: &str = "overwrite_status";

#[derive(Clone, Copy)]
pub enum OperationType {
    Get,
    Put,
    ScrubGet,
}

impl From<OperationType> for ScubaValue {
    fn from(value: OperationType) -> ScubaValue {
        match value {
            OperationType::Get => ScubaValue::from("get"),
            OperationType::Put => ScubaValue::from("put"),
            OperationType::ScrubGet => ScubaValue::from("scrub_get"),
        }
    }
}

fn add_common_values(
    scuba: &mut MononokeScubaSampleBuilder,
    pc: &PerfCounters,
    key: &str,
    session: String,
    stats: FutureStats,
    operation: OperationType,
    blobstore_id: Option<BlobstoreId>,
) {
    scuba
        .add(KEY, key)
        .add(OPERATION, operation)
        .add(COMPLETION_TIME, stats.completion_time.as_micros_unchecked());

    pc.insert_nonzero_perf_counters(scuba);

    if let Some(blobstore_id) = blobstore_id {
        scuba.add(BLOBSTORE_ID, blobstore_id);
    }

    if stats.completion_time >= SLOW_REQUEST_THRESHOLD {
        scuba.add(SESSION, session);
    }
}

pub fn record_get_stats(
    scuba: &mut MononokeScubaSampleBuilder,
    pc: &PerfCounters,
    stats: FutureStats,
    result: Result<&Option<BlobstoreGetData>, &Error>,
    key: &str,
    session: String,
    operation: OperationType,
    blobstore_id: Option<BlobstoreId>,
) {
    add_common_values(scuba, pc, key, session, stats, operation, blobstore_id);

    match result {
        Ok(Some(data)) => {
            let size = data.as_bytes().len();
            let size_logging_threshold = tunables().get_blobstore_read_size_logging_threshold();
            if size_logging_threshold > 0 && size > size_logging_threshold as usize {
                scuba.unsampled();
            }
            scuba.add(SIZE, size);
        }
        Err(error) => {
            // Always log errors
            scuba.unsampled();
            scuba.add(ERROR, format!("{:#}", error));
        }
        Ok(None) => {}
    }

    scuba.log();
}

pub fn record_put_stats(
    scuba: &mut MononokeScubaSampleBuilder,
    pc: &PerfCounters,
    stats: FutureStats,
    result: Result<&OverwriteStatus, &Error>,
    key: &str,
    session: String,
    operation: OperationType,
    size: usize,
    blobstore_id: Option<BlobstoreId>,
    write_order: Option<usize>,
) {
    add_common_values(scuba, pc, key, session, stats, operation, blobstore_id);
    scuba.add(SIZE, size);

    match result {
        Ok(overwrite_status) => {
            scuba.add(OVERWRITE_STATUS, overwrite_status.as_ref());
            if let Some(write_order) = write_order {
                scuba.add(WRITE_ORDER, write_order);
            }
        }
        Err(error) => {
            scuba.add(ERROR, format!("{:#}", error));
        }
    };

    scuba.log();
}
