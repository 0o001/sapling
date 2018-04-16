// Copyright (c) 2018-present, Facebook, Inc.
// All Rights Reserved.
//
// This software may be used and distributed according to the terms of the
// GNU General Public License version 2 or any later version.

extern crate clap;
extern crate db;
extern crate dieselfilenodes;
extern crate filenodes;
extern crate futures;
extern crate mercurial_types;
#[macro_use]
extern crate slog;
extern crate slog_glog_fmt;
extern crate slog_term;
extern crate time_ext;

use dieselfilenodes::{MysqlFilenodes, DEFAULT_INSERT_CHUNK_SIZE};
use filenodes::Filenodes;
use futures::future::Future;
use mercurial_types::{DFileNodeId, DNodeHash, RepoPath, RepositoryId};
use slog::{Drain, Level};
use slog_glog_fmt::default_drain as glog_drain;
use std::str::FromStr;
use std::time::Instant;
use time_ext::DurationExt;

fn main() {
    let matches = clap::App::new("revlog to blob importer")
        .version("0.0.0")
        .about("make blobs")
        .args_from_usage(
            r#"
            [filename]                  'filename'
            [filenode]                  'filenode'
            [xdb-tier]                  'xdb tier'
            --depth [DEPTH]             'how many ancestors to fetch, fetch all if not set'
            --directory                 'not a file but directory'
            --debug
        "#,
        )
        .get_matches();

    let filename = matches
        .value_of("filename")
        .expect("filename is not specified");
    let filenode = matches
        .value_of("filenode")
        .expect("filenode is not specified");
    let xdb_tier = matches
        .value_of("xdb-tier")
        .expect("xdb-tier is not specified");
    let is_directory = matches.is_present("directory");
    let depth: Option<usize> = matches
        .value_of("depth")
        .map(|depth| depth.parse().expect("depth must be a positive integer"));

    let root_log = {
        let level = if matches.is_present("debug") {
            Level::Debug
        } else {
            Level::Info
        };

        let drain = glog_drain().filter_level(level).fuse();
        slog::Logger::root(drain, o![])
    };

    let filename = if is_directory {
        info!(root_log, "directory");
        RepoPath::dir(filename).expect("incorrect repopath")
    } else {
        info!(root_log, "file");
        RepoPath::file(filename).expect("incorrect repopath")
    };
    let filenode_hash = DNodeHash::from_str(filenode).expect("incorrect filenode: should be sha1");

    let mut filenode_hash = DFileNodeId::new(filenode_hash);

    info!(root_log, "Connecting to mysql...");
    let connection_params = db::get_connection_params(
        xdb_tier,
        db::InstanceRequirement::ReplicaOnly,
        None,
        Some(db::ProxyRequirement::Forbidden),
    ).expect("cannot create connection params");
    let filenodes = MysqlFilenodes::open(connection_params, DEFAULT_INSERT_CHUNK_SIZE)
        .expect("cannot connect to mysql");
    info!(root_log, "Connected");

    info!(root_log, "Fetching parents...");
    let before = Instant::now();
    let mut res = 0;
    loop {
        let filenode = filenodes
            .get_filenode(&filename, &filenode_hash, &RepositoryId::new(0))
            .wait()
            .expect("failed to fetch")
            .expect("not found");
        res += 1;
        if Some(res) == depth {
            break;
        }
        match filenode.p1 {
            Some(p1) => {
                filenode_hash = p1;
            }
            None => {
                break;
            }
        }
    }

    info!(
        root_log,
        "Finished: {}, took: {:?}",
        res,
        Instant::now().duration_since(before).as_millis()
    );
}
