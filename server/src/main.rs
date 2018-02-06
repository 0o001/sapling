// Copyright (c) 2004-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

#![deny(warnings)]
#![feature(never_type)]
#![feature(try_from)]

#[macro_use]
extern crate failure_ext as failure;
#[macro_use]
extern crate futures;
extern crate futures_ext;
extern crate tokio_core;
extern crate tokio_io;
extern crate tokio_uds;

extern crate clap;

#[macro_use]
extern crate slog;
extern crate slog_glog_fmt;
extern crate slog_kvfilter;
extern crate slog_logview;
extern crate slog_stats;
extern crate slog_term;

#[macro_use]
extern crate maplit;

extern crate async_compression;
extern crate blobrepo;
extern crate bytes;
extern crate hgproto;
#[cfg(test)]
extern crate many_files_dirs;
extern crate mercurial;
extern crate mercurial_bundles;
extern crate mercurial_types;
#[cfg(test)]
extern crate mercurial_types_mocks;
extern crate metaconfig;
extern crate repoinfo;
extern crate revset;
extern crate services;
extern crate sshrelay;
extern crate stats;

mod errors;
mod repo;
mod listener;

use std::io;
use std::panic;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use failure::SlogKVError;
use futures::{Future, Sink, Stream};
use futures::sink::Wait;
use futures::sync::mpsc;

use clap::{App, ArgGroup, ArgMatches};

use slog::{Drain, Level, Logger};
use slog_glog_fmt::{kv_categorizer, kv_defaults, GlogFormat};
use slog_kvfilter::KVFilter;
use slog_logview::LogViewDrain;

use bytes::Bytes;
use hgproto::{sshproto, HgProtoHandler};
use mercurial::RevlogRepo;
use metaconfig::RepoConfigs;
use metaconfig::repoconfig::RepoType;

use errors::*;

use listener::{ssh_server_mux, Stdio};

struct SenderBytesWrite {
    chan: Wait<mpsc::Sender<Bytes>>,
}

impl io::Write for SenderBytesWrite {
    fn flush(&mut self) -> io::Result<()> {
        self.chan
            .flush()
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.chan
            .send(Bytes::from(buf))
            .map(|_| buf.len())
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

// Exit the whole process if any of the threads fails to catch a panic
fn setup_panic_hook() {
    let original_hook = panic::take_hook();

    panic::set_hook(Box::new(move |info| {
        original_hook(info);
        std::process::exit(1);
    }));
}

fn setup_app<'a, 'b>() -> App<'a, 'b> {
    App::new("mononoke server")
        .version("0.0.0")
        .about("serve repos")
        .args_from_usage(
            r#"
            <crpath>      -P, --configrepo_path [PATH]           'path to the config repo'

            [crbookmark]  -B, --configrepo_bookmark [BOOKMARK]   'config repo bookmark'
            [crhash]      -C, --configrepo_hash [HASH]           'config repo commit hash'

            -p, --thrift_port [PORT] 'if provided the thrift server will start on this port'

            -d, --debug                                          'print debug level output'
        "#,
        )
        .group(
            ArgGroup::default()
                .args(&["crbookmark", "crhash"])
                .required(true),
        )
}

fn setup_logger<'a>(matches: &ArgMatches<'a>) -> Logger {
    let level = if matches.is_present("debug") {
        Level::Debug
    } else {
        Level::Info
    };

    let drain = {
        let drain = {
            // TODO: switch to TermDecorator, which supports color
            let decorator = slog_term::PlainSyncDecorator::new(io::stdout());
            let stderr_drain = GlogFormat::new(decorator, kv_categorizer::FacebookCategorizer);
            let logview_drain = LogViewDrain::new("errorlog_mononoke");
            slog::Duplicate::new(stderr_drain, logview_drain)
        };
        let drain = slog_stats::StatsDrain::new(drain);
        drain.filter_level(level)
    };

    Logger::root(
        drain.fuse(),
        o!(kv_defaults::FacebookKV::new().expect("Failed to initialize logging")),
    )
}

fn start_stats() -> Result<JoinHandle<!>> {
    Ok(thread::Builder::new()
        .name("stats_aggregation".to_owned())
        .spawn(move || {
            let mut core = tokio_core::reactor::Core::new().expect("failed to create tokio core");
            let scheduler = stats::schedule_stats_aggregation(&core.handle())
                .expect("failed to create stats aggregation scheduler");
            core.run(scheduler).expect("stats scheduler failed");
            // stats scheduler shouldn't finish successfully
            unreachable!()
        })?)
}

fn start_thrift_service<'a>(
    logger: &Logger,
    matches: &ArgMatches<'a>,
) -> Option<Result<JoinHandle<!>>> {
    matches.value_of("thrift_port").map(|port| {
        let port = port.parse().expect("Failed to parse thrift_port as number");
        info!(logger, "Initializing thrift server on port {}", port);

        thread::Builder::new()
            .name("thrift_service".to_owned())
            .spawn(move || {
                services::run_service_framework(
                    "mononoke_server",
                    port,
                    0, // Disables separate status http server
                ).expect("failure while running thrift service framework")
            })
            .map_err(Error::from)
    })
}

fn get_config<'a>(logger: &Logger, matches: &ArgMatches<'a>) -> Result<RepoConfigs> {
    // TODO: This needs to cope with blob repos, too
    let mut crpath = PathBuf::from(matches.value_of("crpath").unwrap());
    crpath.push(".hg");
    let config_repo = RevlogRepo::open(crpath)?;

    let changesetid = if let Some(bookmark) = matches.value_of("crbookmark") {
        config_repo
            .get_bookmark_value(&bookmark)
            .wait()?
            .ok_or_else(|| failure::err_msg("bookmark for config repo not found"))?
            .0
    } else {
        mercurial_types::nodehash::ChangesetId::from_str(matches.value_of("crhash").unwrap())?
    };

    info!(
        logger,
        "Config repository will be read from commit: {}", changesetid
    );

    RepoConfigs::read_revlog_config_repo(config_repo, changesetid)
        .from_err()
        .wait()
}

fn start_repo_listeners<I>(repos: I, root_log: &Logger) -> Result<Vec<JoinHandle<!>>>
where
    I: IntoIterator<Item = (RepoType, usize)>,
{
    // Given the list of paths to repos:
    // - create a thread for it
    // - initialize the repo
    // - wait for connections in that thread

    let handles: Vec<_> = repos
        .into_iter()
        .map(move |(repotype, cache_size)| {
            // start a thread for each repo to own the reactor and start listening for
            // connections and detach it
            thread::Builder::new()
                .name(format!("listener_{:?}", repotype))
                .spawn({
                    let root_log = root_log.clone();
                    move || repo_listen(repotype, cache_size, root_log.clone())
                })
                .map_err(Error::from)
        })
        .collect();

    if handles.iter().any(Result::is_err) {
        for err in handles.into_iter().filter_map(Result::err) {
            crit!(root_log, "Failed to spawn listener thread"; SlogKVError(err));
        }
        bail_err!(ErrorKind::Initialization(
            "at least one of the listener threads failed to be spawned",
        ));
    }

    Ok(handles.into_iter().filter_map(Result::ok).collect())
}

// Listener thread for a specific repo
fn repo_listen(repotype: RepoType, cache_size: usize, root_log: Logger) -> ! {
    let mut core = tokio_core::reactor::Core::new().expect("failed to create tokio core");
    let (sockname, repo) = repo::init_repo(&root_log, &repotype, cache_size, &core.remote())
        .expect("failed to initialize repo");

    let listen_log = root_log.new(o!("repo" => repo.path().clone()));

    let handle = core.handle();
    let repo = Arc::new(repo);

    let server = listener::listener(sockname, &handle)
        .expect("failed to create listener")
        .map_err(Error::from)
        .for_each(move |sock| {
            match sock.peer_addr() {
                Ok(addr) => info!(listen_log, "New connection from {:?}", addr),
                Err(err) => {
                    error!(listen_log, "Failed to get peer addr"; SlogKVError(Error::from(err)))
                }
            };

            // Have a connection. Extract std{in,out,err} streams for socket
            let Stdio {
                stdin,
                stdout,
                stderr,
            } = ssh_server_mux(sock, &handle);

            let stderr_write = SenderBytesWrite {
                chan: stderr.clone().wait(),
            };
            let drain = slog_term::PlainSyncDecorator::new(stderr_write);
            let drain = slog_term::FullFormat::new(drain).build();
            let drain = KVFilter::new(drain, Level::Critical).only_pass_any_on_all_keys(hashmap! {
                "remote".into() => hashset!["true".into()],
            });
            let drain = slog::Duplicate::new(drain, listen_log.clone()).fuse();
            let conn_log = Logger::root(drain, o![]);

            // Construct a hg protocol handler
            let proto_handler = HgProtoHandler::new(
                stdin,
                repo::RepoClient::new(repo.clone(), &conn_log),
                sshproto::HgSshCommandDecode,
                sshproto::HgSshCommandEncode,
                &conn_log,
            );

            // send responses back
            let endres = proto_handler
                .map_err(Error::from)
                .forward(stdout)
                .map(|_| ());

            // If we got an error at this point, then catch it, print a message and return
            // Ok (if we allow the Error to propagate further it will shutdown the listener
            // rather than just the connection). Unfortunately there's no way to print what the
            // actual failing command was.
            // TODO: seems to leave the client hanging?
            let conn_log = conn_log.clone();
            let endres = endres.or_else(move |err| {
                error!(conn_log, "Command failed"; SlogKVError(err), "remote" => "true");
                Ok(())
            });

            // Run the whole future asynchronously to allow new connections
            handle.spawn(endres);

            Ok(())
        });

    core.run(server)
        .expect("failure while running listener on tokio core");

    // The server is an infinite stream of connections
    unreachable!();
}

fn main() {
    setup_panic_hook();
    let matches = setup_app().get_matches();
    let root_log = setup_logger(&matches);

    fn run_server<'a>(root_log: &Logger, matches: ArgMatches<'a>) -> Result<!> {
        info!(root_log, "Starting up");

        let stats_aggregation = start_stats()?;
        let maybe_thrift = match start_thrift_service(&root_log, &matches) {
            None => None,
            Some(handle) => Some(handle?),
        };

        let config = get_config(root_log, &matches)?;
        let repo_listeners = start_repo_listeners(
            config
                .repos
                .into_iter()
                .map(|(_, c)| (c.repotype, c.generation_cache_size)),
            root_log,
        )?;

        for handle in vec![stats_aggregation]
            .into_iter()
            .chain(maybe_thrift.into_iter())
            .chain(repo_listeners.into_iter())
        {
            let thread_name = handle.thread().name().unwrap_or("unknown").to_owned();
            match handle.join() {
                Err(panic) => crit!(root_log, "Thread {} paniced with: {:?}", thread_name, panic),
            }
        }

        info!(root_log, "No service to run, shutting down");
        std::process::exit(0);
    }

    match run_server(&root_log, matches) {
        Err(e) => {
            crit!(root_log, "Server fatal error"; SlogKVError(e));
            std::process::exit(1);
        }
    }
}
