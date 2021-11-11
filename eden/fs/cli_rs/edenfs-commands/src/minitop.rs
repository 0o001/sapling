/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

//! edenfsctl minitop

use async_trait::async_trait;
use crossterm::terminal;
use crossterm::{cursor, execute, QueueableCommand};
use std::collections::BTreeMap;
use std::io::{stdout, Write};
use std::path::Path;
use std::time::Duration;
use std::time::SystemTime;
use structopt::StructOpt;

use anyhow::anyhow;
use edenfs_client::EdenFsInstance;
use edenfs_error::{Result, ResultExt};

use thrift_types::edenfs::types::{pid_t, AccessCounts, GetAccessCountsResult};

use crate::ExitCode;

#[derive(StructOpt, Debug)]
#[structopt(about = "Simple monitoring of EdenFS accesses by process.")]
pub struct MinitopCmd {
    // TODO: For minitop, we may want to allow querying for < 1s, but this
    // requires modifying the thrift call and the eden service itself.
    // < 1s may be more useful for the realtime stats we see in minitop/top.
    #[structopt(
        long,
        short,
        help = "Specify the rate (in seconds) at which eden top updates.",
        default_value = "1",
        parse(from_str = parse_refresh_rate),
    )]
    refresh_rate: Duration,
}

fn parse_refresh_rate(arg: &str) -> Duration {
    let seconds = arg
        .parse::<u64>()
        .expect("Please enter a valid whole positive number for refresh_rate.");

    Duration::new(seconds, 0)
}

const UNKNOWN_COMMAND: &str = "<unknown>";
const COLUMN_TITLES: &[&str] = &[
    "TOP PID",
    "MOUNT",
    "FUSE R",
    "FUSE W",
    "FUSE COUNT",
    "FUSE FETCH",
    "MEMORY",
    "DISK",
    "IMPORTS",
    "FUSE TIME",
    "FUSE LAST",
    "CMD",
];

trait GetAccessCountsResultExt {
    fn get_cmd_for_pid(&self, pid: &i32) -> Result<String>;
}

impl GetAccessCountsResultExt for GetAccessCountsResult {
    fn get_cmd_for_pid(&self, pid: &i32) -> Result<String> {
        match self.cmdsByPid.get(pid) {
            Some(cmd) => Ok(String::from_utf8(cmd.to_vec()).from_err()?),
            None => Ok(String::from(UNKNOWN_COMMAND)),
        }
    }
}

trait AccessCountsExt {
    fn add(&mut self, other: &AccessCounts);
}

impl AccessCountsExt for AccessCounts {
    fn add(&mut self, other: &AccessCounts) {
        self.fsChannelTotal += other.fsChannelTotal;
        self.fsChannelReads += other.fsChannelReads;
        self.fsChannelWrites += other.fsChannelWrites;
        self.fsChannelBackingStoreImports += other.fsChannelBackingStoreImports;
        self.fsChannelDurationNs += other.fsChannelDurationNs;
        self.fsChannelMemoryCacheImports += other.fsChannelMemoryCacheImports;
        self.fsChannelDiskCacheImports += other.fsChannelDiskCacheImports;
    }
}

#[derive(Clone)]
struct Process {
    pid: u32,
    mount: String,
    cmd: String,
    access_counts: AccessCounts,
    fetch_counts: u64,
    last_access_time: SystemTime,
}

impl Process {
    fn is_running(&self) -> bool {
        Path::new(&format!("/proc/{}", self.pid)).is_dir()
    }
}

struct TrackedProcesses {
    processes: BTreeMap<u32, Process>,
}

impl TrackedProcesses {
    fn new() -> Self {
        TrackedProcesses {
            processes: BTreeMap::<u32, Process>::new(),
        }
    }

    fn extract_mount(path: &[u8]) -> anyhow::Result<String> {
        let path = std::str::from_utf8(path)?;
        let path = Path::new(&path);
        let filename = path
            .file_name()
            .ok_or_else(|| anyhow!("filename is missing"))?;

        filename
            .to_os_string()
            .into_string()
            .map_err(|_| anyhow!("mount name is not UTF-8"))
    }

    /// Starts to track a given process. If the process is already being tracked,
    /// then it updates the process's information (counts, last update time, etc).
    ///
    /// At any given time, a single pid may have multiple access logs.
    fn update_process(
        &mut self,
        pid: &pid_t,
        mount: &[u8],
        cmd: String,
        access_counts: AccessCounts,
        fetch_counts: &i64,
    ) -> Result<()> {
        let pid = u32::try_from(*pid).from_err()?;
        let mount = TrackedProcesses::extract_mount(mount)?;
        let fetch_counts = u64::try_from(*fetch_counts).from_err()?;

        match self.processes.get_mut(&pid) {
            Some(existing_proc) => {
                existing_proc.cmd = cmd;

                // We increment access counts, but overwrite fetch counts
                // (this matches behavior in original python implementation)
                existing_proc.access_counts.add(&access_counts);
                existing_proc.fetch_counts = fetch_counts;

                existing_proc.last_access_time = SystemTime::now();
            }
            None => {
                self.processes.insert(
                    pid,
                    Process {
                        pid,
                        mount,
                        cmd,
                        access_counts,
                        fetch_counts,
                        last_access_time: SystemTime::now(),
                    },
                );
            }
        }

        Ok(())
    }

    /// We aggregate all tracked processes in a separate step right before rendering
    /// (as opposed to aggregating eagerly as we receive process logs in `update_process`)
    /// because tracked processes could stop running which may change the top_pid.
    fn aggregated_processes(&self) -> Vec<Process> {
        // Technically, it's more correct to aggregate this by TGID
        // Because that's hard to get, we instead aggregate by mount & cmd
        // (mount, cmd) => Process
        let mut aggregated_processes = BTreeMap::<(&str, &str), Process>::new();

        for (_pid, process) in self.processes.iter() {
            match aggregated_processes.get_mut(&(&process.mount, &process.cmd)) {
                Some(agg_proc) => {
                    // We aggregate access counts, but we don't change fetch counts
                    // (this matches behavior in original python implementation)
                    agg_proc.access_counts.add(&process.access_counts);

                    // Figure out what the most relevant process id is
                    if process.is_running() || agg_proc.last_access_time < process.last_access_time
                    {
                        agg_proc.pid = process.pid;
                        agg_proc.last_access_time = process.last_access_time;
                    }
                }
                None => {
                    aggregated_processes.insert((&process.mount, &process.cmd), process.clone());
                }
            }
        }

        aggregated_processes.into_values().collect()
    }
}

#[async_trait]
impl crate::Subcommand for MinitopCmd {
    async fn run(&self, instance: EdenFsInstance) -> Result<ExitCode> {
        let client = instance.connect(None).await?;
        let mut tracked_processes = TrackedProcesses::new();

        // Setup rendering
        let mut stdout = stdout();
        execute!(stdout, cursor::Hide, terminal::DisableLineWrap);

        loop {
            // Update currently tracked processes (and add new ones if they haven't been tracked yet)
            let counts = client
                .getAccessCounts(self.refresh_rate.as_secs().try_into().from_err()?)
                .await
                .from_err()?;

            for (mount, accesses) in &counts.accessesByMount {
                for (pid, access_counts) in &accesses.accessCountsByPid {
                    tracked_processes.update_process(
                        pid,
                        mount,
                        counts.get_cmd_for_pid(pid)?,
                        access_counts.clone(),
                        accesses.fetchCountsByPid.get(pid).unwrap_or(&0),
                    )?;
                }

                for (pid, fetch_counts) in &accesses.fetchCountsByPid {
                    tracked_processes.update_process(
                        pid,
                        mount,
                        counts.get_cmd_for_pid(pid)?,
                        AccessCounts::default(),
                        fetch_counts,
                    )?;
                }
            }

            // Render aggregated processes
            stdout.queue(cursor::SavePosition).from_err()?;

            stdout
                .write(format!("{}\n", COLUMN_TITLES.join("\t")).as_bytes())
                .from_err()?;
            for aggregated_process in tracked_processes.aggregated_processes() {
                stdout
                    .write(
                        format!(
                            "{top_pid}\t \
                            {mount}\t \
                            {fuse_reads}\t \
                            {fuse_writes}\t \
                            {fuse_total}\t \
                            {fuse_fetch}\t \
                            {fuse_memory_cache_imports}\t \
                            {fuse_disk_cache_imports}\t \
                            {fuse_backing_store_imports}\t \
                            {fuse_duration}\t \
                            {fuse_last_access}\t \
                            {command}\n",
                            top_pid = aggregated_process.pid,
                            mount = aggregated_process.mount,
                            fuse_reads = aggregated_process.access_counts.fsChannelReads,
                            fuse_writes = aggregated_process.access_counts.fsChannelWrites,
                            fuse_total = aggregated_process.access_counts.fsChannelTotal,
                            fuse_fetch = aggregated_process.fetch_counts,
                            fuse_memory_cache_imports =
                                aggregated_process.access_counts.fsChannelMemoryCacheImports,
                            fuse_disk_cache_imports =
                                aggregated_process.access_counts.fsChannelDiskCacheImports,
                            fuse_backing_store_imports = aggregated_process
                                .access_counts
                                .fsChannelBackingStoreImports,
                            fuse_duration = aggregated_process.access_counts.fsChannelDurationNs,
                            fuse_last_access = aggregated_process
                                .last_access_time
                                .elapsed()
                                .from_err()?
                                .as_nanos(),
                            command = aggregated_process.cmd,
                        )
                        .as_bytes(),
                    )
                    .from_err()?;
            }
            stdout.queue(cursor::RestorePosition).from_err()?;
            stdout.flush().from_err()?;

            tokio::time::sleep(self.refresh_rate).await;
        }

        unreachable!("minitop is unable to start");

        Ok(0)
    }
}
