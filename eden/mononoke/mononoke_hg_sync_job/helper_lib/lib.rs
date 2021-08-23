/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use anyhow::{format_err, Error, Result};
use blobrepo::BlobRepo;
use blobstore::{Blobstore, Loadable};
use bookmarks::Freshness;
use context::CoreContext;
use futures::{compat::Future01CompatExt, stream::TryStreamExt, Future};
use mononoke_types::RawBundle2Id;
use mutable_counters::MutableCounters;
use slog::{info, Logger};
use std::convert::TryInto;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::{
    fs::{File as AsyncFile, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    time::{self, sleep, timeout},
};

pub const LATEST_REPLAYED_REQUEST_KEY: &str = "latest-replayed-request";

pub async fn save_bundle_to_temp_file<B: Blobstore>(
    ctx: &CoreContext,
    blobstore: &B,
    bundle2_id: RawBundle2Id,
) -> Result<NamedTempFile, Error> {
    let tempfile = NamedTempFile::new()?;

    save_bundle_to_file(
        ctx,
        blobstore,
        bundle2_id,
        tempfile.path().to_path_buf(),
        false, /* create */
    )
    .await?;

    Ok(tempfile)
}

pub async fn save_bundle_to_file<B: Blobstore>(
    ctx: &CoreContext,
    blobstore: &B,
    bundle2_id: RawBundle2Id,
    file: PathBuf,
    create: bool,
) -> Result<(), Error> {
    let bytes = bundle2_id.load(ctx, blobstore).await?;
    save_bytes_to_file(bytes.into_bytes(), file, create).await?;

    Ok(())
}

pub async fn save_bytes_to_temp_file<B: AsRef<[u8]>>(bytes: B) -> Result<NamedTempFile, Error> {
    let tempfile = NamedTempFile::new()?;
    save_bytes_to_file(
        bytes,
        tempfile.path().to_path_buf(),
        false, /* create */
    )
    .await?;
    Ok(tempfile)
}

pub async fn save_bytes_to_file<B: AsRef<[u8]>>(
    bytes: B,
    file: PathBuf,
    create: bool,
) -> Result<(), Error> {
    let mut file = OpenOptions::new()
        .create(create)
        .write(true)
        .open(file)
        .await?;

    file.write_all(bytes.as_ref()).await?;
    file.flush().await?;

    Ok(())
}

pub async fn write_to_named_temp_file<B>(bytes: B) -> Result<NamedTempFile, Error>
where
    B: AsRef<[u8]>,
{
    let tempfile = NamedTempFile::new()?;
    let mut file = open_tempfile(&tempfile).await?;

    file.write_all(bytes.as_ref()).await?;
    file.flush().await?;

    Ok(tempfile)
}

async fn open_tempfile(tempfile: &NamedTempFile) -> Result<AsyncFile, Error> {
    let file = OpenOptions::new()
        .write(true)
        .open(tempfile.path().to_path_buf())
        .await?;

    Ok(file)
}

/// Get lines after the first `num` lines in file
pub async fn lines_after(p: impl AsRef<Path>, num: usize) -> Result<Vec<String>, Error> {
    let file = AsyncFile::open(p).await?;
    let reader = BufReader::new(file);
    let mut v: Vec<_> = tokio_stream::wrappers::LinesStream::new(reader.lines())
        .try_collect()
        .await?;
    Ok(v.split_off(num))
}

/// Wait until the file has more than `initial_num` lines, then return new lines
/// Timeout after `timeout_millis` ms.
pub async fn wait_till_more_lines(
    p: impl AsRef<Path>,
    initial_num: usize,
    timeout_millis: u64,
) -> Result<Vec<String>, Error> {
    let p = p.as_ref().to_path_buf();

    let read = async {
        loop {
            let new_lines = lines_after(p.clone(), initial_num).await?;
            let new_num = new_lines.len();
            let stop = new_num > 0;
            if stop {
                return Ok(new_lines);
            }

            sleep(Duration::from_millis(100)).await;
        }
    };

    match timeout(Duration::from_millis(timeout_millis), read).await {
        Ok(Ok(lines)) => Ok(lines),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Error::msg("timed out waiting for new lines")),
    }
}

pub fn read_file_contents<F: Seek + Read>(f: &mut F) -> Result<String> {
    // NOTE: Normally (for our use case at this time), we don't advance our position in this file,
    // but let's be conservative and seek to the start anyway.
    let pos = SeekFrom::Start(0);
    f.seek(pos)
        .map_err(|e| format_err!("could not seek to {:?}: {:?}", pos, e))?;

    let mut buff = vec![];
    f.read_to_end(&mut buff)
        .map_err(|e| format_err!("could not read: {:?}", e))?;

    String::from_utf8(buff).map_err(|e| format_err!("log file is not valid utf-8: {:?}", e))
}

#[derive(Copy, Clone)]
pub struct RetryAttemptsCount(pub usize);

pub async fn retry<V, Fut, Func>(
    logger: &Logger,
    func: Func,
    base_retry_delay_ms: u64,
    retry_num: usize,
) -> Result<(V, RetryAttemptsCount), Error>
where
    V: Send + 'static,
    Fut: Future<Output = Result<V, Error>>,
    Func: Fn(usize) -> Fut + Send,
{
    let mut attempt = 1;
    loop {
        let res = func(attempt).await;
        match res {
            Ok(res) => {
                return Ok((res, RetryAttemptsCount(attempt)));
            }
            Err(err) => {
                if attempt >= retry_num {
                    return Err(err);
                }
                info!(
                    logger,
                    "retrying attempt {} of {}...",
                    attempt + 1,
                    retry_num
                );

                let delay = Duration::from_millis(base_retry_delay_ms * 2u64.pow(attempt as u32));
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

/// Wait until all of the entries in the queue have been synced to hg
pub async fn wait_for_latest_log_id_to_be_synced<C>(
    ctx: &CoreContext,
    repo: &BlobRepo,
    mutable_counters: &C,
    sleep_secs: u64,
) -> Result<(), Error>
where
    C: MutableCounters + Clone + Sync + 'static,
{
    wait_for_latest_log_id_for_repo_to_be_synced(ctx, repo, repo, mutable_counters, sleep_secs)
        .await
}

pub async fn wait_for_latest_log_id_for_repo_to_be_synced<C>(
    ctx: &CoreContext,
    repo: &BlobRepo,
    target_repo: &BlobRepo,
    mutable_counters: &C,
    sleep_secs: u64,
) -> Result<(), Error>
where
    C: MutableCounters + Clone + Sync + 'static,
{
    let target_repo_id = target_repo.get_repoid();
    let largest_id = match repo
        .bookmark_update_log()
        .get_largest_log_id(ctx.clone(), Freshness::MostRecent)
        .await?
    {
        Some(id) => id,
        None => return Err(format_err!("Couldn't fetch id from bookmarks update log")),
    };

    /*
        In mutable counters table we store the latest bookmark id replayed by mercurial with
        LATEST_REPLAYED_REQUEST_KEY key. We use this key to extract the latest replayed id
        and compare it with the largest bookmark log id after we move the bookmark.
        If the replayed id is larger or equal to the bookmark id, we can try to move the bookmark
        to the next batch of commits
    */

    loop {
        let mut_counters_value = match mutable_counters
            .get_counter(ctx.clone(), target_repo_id, LATEST_REPLAYED_REQUEST_KEY)
            .compat()
            .await?
        {
            Some(value) => value,
            None => {
                return Err(format_err!(
                    "Couldn't fetch the counter value from mutable_counters for repo_id {:?}",
                    target_repo_id
                ));
            }
        };
        if largest_id > mut_counters_value.try_into().unwrap() {
            info!(
                ctx.logger(),
                "Waiting for {} to be replayed to hg, the latest replayed is {}, repo: {}",
                largest_id,
                mut_counters_value,
                target_repo.name(),
            );
            time::sleep(time::Duration::from_secs(sleep_secs)).await;
        } else {
            break;
        }
    }

    Ok(())
}
