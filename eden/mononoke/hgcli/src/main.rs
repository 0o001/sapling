/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]

use anyhow::Error;
use clap::{App, Arg, ArgGroup, SubCommand};
use fbinit::FacebookInit;
use std::time::Duration;

mod serve;

#[fbinit::main]
fn main(fb: FacebookInit) {
    let matches = App::new("Mononoke CLI")
        .about("Provide minimally compatible CLI to Mononoke server")
        .arg(Arg::from_usage("-R, --repository=<REPO> 'repository name'"))
        .arg(Arg::from_usage(
            "--query-string [QUERY_STRING] 'original query string passed to repository path'",
        ))
        .arg(Arg::from_usage("--remote-proxy 'hgcli is run as remote proxy, not locally'"))
        .arg(Arg::from_usage(
            "--scuba-table [SCUBA_TABLE] 'name of scuba table to log to'",
        ))
        .arg(Arg::from_usage(
            "--no-session-output 'disables the session uuid output'",
        ))
        .arg(
            Arg::with_name("priority")
                .long("priority")
                .takes_value(true)
                .required(false)
                .help("Set request priority"),
        )
        .subcommand(
            SubCommand::with_name("serve")
                .about("start server")
                .arg(Arg::from_usage(
                    "--mononoke-path <PATH> 'path to connect to mononoke server'",
                ))
                .arg(Arg::from_usage(
                    "-A, --accesslog [FILE] 'name of access log file'",
                ))
                .arg(Arg::from_usage("-d, --daemon 'run server in background'"))
                .arg(Arg::from_usage(
                    "-E, --errorlog [FILE] 'name of error log file to write to'",
                ))
                .arg(Arg::from_usage("-p, --port <PORT> 'port to listen on'").default_value("8000"))
                .arg(Arg::from_usage(
                    "-a, --address [ADDR] 'address to listen on'",
                ))
                .arg(Arg::from_usage(
                    "--cert [CERT]  'path to the certificate file'",
                ))
                .arg(Arg::from_usage("--ca-pem [PEM] 'path to the pem file'"))
                .arg(Arg::from_usage(
                    "--private-key [KEY] 'path to the private key'",
                ))
                .arg(Arg::from_usage("--insecure 'run hgcli without verifying peer certificate'"))
                .arg(Arg::from_usage("--stdio 'for remote clients'"))
                .arg(
                    Arg::from_usage("--cmdserver [MODE] 'for remote clients'")
                        .possible_values(&["pipe", "unix"]),
                )
                .arg(Arg::from_usage(
                    "--mock-username [USERNAME] 'use only in tests, send this username instead of the currently logged in'",
                ))
                .arg(Arg::from_usage(
                    "--client-debug 'tell mononoke to send debug information to the client'",
                ))
                .arg(
                    Arg::with_name(serve::ARG_COMMON_NAME)
                        .long(serve::ARG_COMMON_NAME)
                        .takes_value(true)
                        .required(false)
                        .help("expected SSL common name of the server see https://www.ssl.com/faqs/common-name/"),
                )
                .arg(
                    Arg::with_name(serve::ARG_SERVER_CERT_IDENTITY)
                        .long(serve::ARG_SERVER_CERT_IDENTITY)
                        .takes_value(true)
                        .required(false)
                        .help("expected identity of the server"),
                )
                .group(
                    ArgGroup::with_name("idents")
                        .args(&[serve::ARG_COMMON_NAME, serve::ARG_SERVER_CERT_IDENTITY])
                        .required(true)
                ),
        )
        .get_matches();

    let res = if let Some(subcmd) = matches.subcommand_matches("serve") {
        tokio::runtime::Runtime::new()
            .map_err(Error::from)
            .and_then(|mut runtime| {
                // NOTE: We shutdown immediately here. This is a bit unfortunate, but it's due to
                // the fact that we use Stdin / Stdout and Stderr from Tokio, and those things
                // spawn blocking threads, which will prevent the runtime from shutting down. This
                // would normally be OK, but hgcli is a bit special in the sense that the client
                // does not close stdin, so stdin will usually be blocked waiting for input. Note
                // that if serve has exited, there is nothing left to do, since it means
                // - The client may have closed stdin, in which case we don't really have anything
                // to say to them anymore.
                // - The server may have closed the connection, in which case reading from stdin
                // doesn't make any sense.
                let result = runtime.block_on(serve::cmd(fb, &matches, subcmd));
                runtime.shutdown_timeout(Duration::from_nanos(0));
                result
            })
    } else {
        Err(Error::msg("unexpected or missing subcommand"))
    };

    if let Err(err) = res {
        eprintln!("Subcommand failed: {:?}", err);
    }
}
