/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#![deny(warnings)]
#![feature(never_type)]

use anyhow::Result;
use clap::App;
use cloned::cloned;
use cmdlib::{args, monitoring::ReadyFlagService};
use fbinit::FacebookInit;
use futures::channel::oneshot;
use slog::{error, info};

#[cfg(fbcode_build)]
use openssl as _; // suppress unused crate warning - only used outside fbcode

fn setup_app<'a, 'b>() -> App<'a, 'b> {
    let app = args::MononokeAppBuilder::new("mononoke server")
        .with_shutdown_timeout_args()
        .with_all_repos()
        .with_disabled_hooks_args()
        .build()
        .about("serve repos")
        .args_from_usage(
            r#"
            --listening-host-port <PATH>           'tcp address to listen to in format `host:port`'

            -p, --thrift_port [PORT] 'if provided the thrift server will start on this port'

            <cert>        --cert [PATH]                         'path to a file with certificate'
            <private_key> --private-key [PATH]                  'path to a file with private key'
            <ca_pem>      --ca-pem [PATH]                       'path to a file with CA certificate'
            [ticket_seed] --ssl-ticket-seeds [PATH]             'path to a file with encryption keys for SSL tickets'
            "#,
        );

    let app = args::add_mcrouter_args(app);
    let app = args::add_scribe_logging_args(app);
    app
}

#[fbinit::main]
fn main(fb: FacebookInit) -> Result<()> {
    let matches = setup_app().get_matches();
    cmdlib::args::maybe_enable_mcrouter(fb, &matches);

    let (caching, root_log, runtime) = cmdlib::args::init_mononoke(fb, &matches)?;
    let config_source = cmdlib::args::init_config_store(fb, &root_log, &matches)?;

    info!(root_log, "Starting up");

    let config = args::load_repo_configs(config_source, &matches)?;
    let acceptor = {
        let cert = matches.value_of("cert").unwrap().to_string();
        let private_key = matches.value_of("private_key").unwrap().to_string();
        let ca_pem = matches.value_of("ca_pem").unwrap().to_string();

        secure_utils::SslConfig::new(
            ca_pem,
            cert,
            private_key,
            matches.value_of("ssl-ticket-seeds"),
        )
        .build_tls_acceptor(root_log.clone())
        .expect("failed to build tls acceptor")
    };

    info!(root_log, "Creating repo listeners");

    let service = ReadyFlagService::new();
    let (terminate_sender, terminate_receiver) = oneshot::channel::<()>();

    let repo_listeners = {
        cloned!(root_log);

        repo_listener::create_repo_listeners(
            fb,
            cmdlib::args::is_test_instance(&matches),
            config.common,
            config.repos.into_iter(),
            cmdlib::args::parse_mysql_options(&matches),
            caching,
            cmdlib::args::parse_disabled_hooks_with_repo_prefix(&matches, &root_log)?,
            root_log,
            matches
                .value_of("listening-host-port")
                .expect("listening path must be specified")
                .to_string(),
            acceptor,
            service.clone(),
            terminate_receiver,
            config_source,
            cmdlib::args::parse_readonly_storage(&matches),
            cmdlib::args::parse_blobstore_options(&matches),
            cmdlib::args::get_scribe(fb, &matches)?,
        )
    };

    #[cfg(fbcode_build)]
    {
        tracing_fb303::register(fb);
    }

    // Thread with a thrift service is now detached
    monitoring::start_thrift_service(fb, &root_log, &matches, service);

    cmdlib::helpers::serve_forever(
        runtime,
        repo_listeners,
        &root_log,
        || {},
        args::get_shutdown_grace_period(&matches)?,
        async {
            match terminate_sender.send(()) {
                Err(err) => error!(root_log, "could not send termination signal: {:?}", err),
                _ => {}
            }
            repo_listener::wait_for_connections_closed().await;
        },
        args::get_shutdown_timeout(&matches)?,
    )
}
