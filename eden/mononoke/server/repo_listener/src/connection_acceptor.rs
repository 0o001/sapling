/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use hostname::get_hostname;
use hyper::server::conn::Http;
use session_id::generate_session_id;
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Error, Result};
use bytes::Bytes;
use cached_config::ConfigStore;
use edenapi_service::EdenApi;
use failure_ext::SlogKVError;
use fbinit::FacebookInit;
use futures::{channel::oneshot, future::Future, select_biased};
use futures_01_ext::BoxStream;
use futures_ext::FbFutureExt;
use futures_old::{stream, sync::mpsc, Stream};
use futures_util::compat::Stream01CompatExt;
use futures_util::future::{AbortHandle, FutureExt};
use futures_util::stream::{StreamExt, TryStreamExt};
use lazy_static::lazy_static;
use load_limiter::LoadLimiterEnvironment;
use metaconfig_types::CommonConfig;
use openssl::ssl::{Ssl, SslAcceptor};
use permission_checker::{MononokeIdentity, MononokeIdentitySet};
use scribe_ext::Scribe;
use slog::{debug, error, info, warn, Logger};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_openssl::SslStream;
use tokio_util::codec::{FramedRead, FramedWrite};

use cmdlib::monitoring::ReadyFlagService;
use sshrelay::{
    IoStream, Metadata, Preamble, Priority, SshDecoder, SshEncoder, SshEnvVars, SshMsg, Stdio,
};
use stats::prelude::*;

use crate::errors::ErrorKind;
use crate::http_service::MononokeHttpService;
use crate::repo_handlers::RepoHandler;
use crate::request_handler::{create_conn_logger, request_handler};
use crate::security_checker::ConnectionsSecurityChecker;
use qps::Qps;
use quiet_stream::QuietShutdownStream;

define_stats! {
    prefix = "mononoke.connection_acceptor";
    http_accepted: timeseries(Sum),
    hgcli_accepted: timeseries(Sum),
}

pub trait MononokeStream: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}

impl<T> MononokeStream for T where T: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static {}

const KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(5000);
const CHUNK_SIZE: usize = 10000;
lazy_static! {
    static ref OPEN_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
}

pub async fn wait_for_connections_closed(logger: &Logger) {
    loop {
        let conns = OPEN_CONNECTIONS.load(Ordering::Relaxed);
        if conns == 0 {
            break;
        }

        slog::info!(logger, "Waiting for {} connections to close", conns);
        tokio::time::sleep(Duration::new(1, 0)).await;
    }
}

/// This function accepts connections, reads Preamble and routes first_line to a thread responsible for
/// a particular repo
pub async fn connection_acceptor(
    fb: FacebookInit,
    common_config: CommonConfig,
    sockname: String,
    service: ReadyFlagService,
    root_log: Logger,
    repo_handlers: HashMap<String, RepoHandler>,
    tls_acceptor: SslAcceptor,
    terminate_process: oneshot::Receiver<()>,
    load_limiter: Option<LoadLimiterEnvironment>,
    scribe: Scribe,
    edenapi: EdenApi,
    will_exit: Arc<AtomicBool>,
    config_store: &ConfigStore,
    cslb_config: Option<String>,
) -> Result<()> {
    let enable_http_control_api = common_config.enable_http_control_api;

    let security_checker =
        ConnectionsSecurityChecker::new(fb, common_config, &repo_handlers, &root_log).await?;
    let addr: SocketAddr = sockname.parse()?;
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("could not bind mononoke on '{}'", sockname))?;

    let mut terminate_process = terminate_process.fuse();

    let qps = match cslb_config {
        Some(config) => Some(Arc::new(
            Qps::new(fb, config, config_store).with_context(|| "Failed to initialize QPS")?,
        )),
        None => None,
    };

    // Now that we are listening and ready to accept connections, report that we are alive.
    service.set_ready();

    let acceptor = Arc::new(Acceptor {
        fb,
        tls_acceptor,
        repo_handlers,
        security_checker,
        load_limiter,
        scribe,
        logger: root_log.clone(),
        edenapi,
        enable_http_control_api,
        server_hostname: get_hostname().unwrap_or_else(|_| "unknown_hostname".to_string()),
        will_exit,
        config_store: config_store.clone(),
        qps,
    });

    loop {
        select_biased! {
            _ = terminate_process => {
                debug!(root_log, "Received shutdown handler, stop accepting connections...");
                return Ok(());
            },
            sock_tuple = listener.accept().fuse() => match sock_tuple {
                Ok((stream, addr)) => {
                    let conn = PendingConnection { acceptor: acceptor.clone(), addr };
                    let task = handle_connection(conn.clone(), stream);
                    conn.spawn_task(task, "Failed to handle_connection");
                }
                Err(err) => {
                    error!(root_log, "{}", err.to_string(); SlogKVError(Error::from(err)));
                }
            },
        };
    }
}

/// Our environment for accepting connections.
pub struct Acceptor {
    pub fb: FacebookInit,
    pub tls_acceptor: SslAcceptor,
    pub repo_handlers: HashMap<String, RepoHandler>,
    pub security_checker: ConnectionsSecurityChecker,
    pub load_limiter: Option<LoadLimiterEnvironment>,
    pub scribe: Scribe,
    pub logger: Logger,
    pub edenapi: EdenApi,
    pub enable_http_control_api: bool,
    pub server_hostname: String,
    pub will_exit: Arc<AtomicBool>,
    pub config_store: ConfigStore,
    pub qps: Option<Arc<Qps>>,
}

/// Details for a socket we've just opened.
#[derive(Clone)]
pub struct PendingConnection {
    pub acceptor: Arc<Acceptor>,
    pub addr: SocketAddr,
}

/// A connection where we completed the initial TLS handshake.
#[derive(Clone)]
pub struct AcceptedConnection {
    pub pending: PendingConnection,
    pub is_trusted: bool,
    pub identities: Arc<MononokeIdentitySet>,
}

impl PendingConnection {
    /// Spawn a task that is dedicated to this connection. This will block server shutdown, and
    /// also log on error or cancellation.
    pub fn spawn_task(
        &self,
        task: impl Future<Output = Result<()>> + Send + 'static,
        label: &'static str,
    ) {
        let this = self.clone();

        OPEN_CONNECTIONS.fetch_add(1, Ordering::Relaxed);

        tokio::task::spawn(async move {
            let logger = &this.acceptor.logger;
            let res = task
                .on_cancel(|| warn!(logger, "connection to {} was cancelled", this.addr))
                .await
                .context(label)
                .with_context(|| format!("Failed to handle connection to {}", this.addr));

            if let Err(e) = res {
                error!(logger, "connection_acceptor error: {:#}", e);
            }

            OPEN_CONNECTIONS.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

async fn handle_connection(conn: PendingConnection, sock: TcpStream) -> Result<()> {
    let ssl = Ssl::new(conn.acceptor.tls_acceptor.context()).context("Error creating Ssl")?;
    let ssl_socket = SslStream::new(ssl, sock).context("Error creating SslStream")?;
    let mut ssl_socket = Box::pin(ssl_socket);

    ssl_socket
        .as_mut()
        .accept()
        .await
        .context("Failed to perform tls handshake")?;

    let identities = match ssl_socket.ssl().peer_certificate() {
        Some(cert) => MononokeIdentity::try_from_x509(&cert),
        None => Err(ErrorKind::ConnectionNoClientCertificate.into()),
    }?;

    let is_trusted = conn
        .acceptor
        .security_checker
        .check_if_trusted(&identities)
        .await?;

    let conn = AcceptedConnection {
        pending: conn,
        is_trusted,
        identities: Arc::new(identities),
    };

    let is_hgcli = ssl_socket.ssl().selected_alpn_protocol() == Some(alpn::HGCLI_ALPN.as_bytes());

    let ssl_socket = QuietShutdownStream::new(ssl_socket);

    if is_hgcli {
        handle_hgcli(conn, ssl_socket)
            .await
            .context("Failed to handle_hgcli")?;
    } else {
        handle_http(conn, ssl_socket)
            .await
            .context("Failed to handle_http")?;
    }

    Ok(())
}

async fn handle_hgcli<S: MononokeStream>(conn: AcceptedConnection, stream: S) -> Result<()> {
    STATS::hgcli_accepted.add_value(1);

    let (rx, tx) = tokio::io::split(stream);

    let mut framed = FramedConn::setup(rx, tx);

    let preamble = match framed.rd.next().await.transpose()? {
        Some(maybe_preamble) => {
            if let IoStream::Preamble(preamble) = maybe_preamble.stream() {
                preamble
            } else {
                return Err(ErrorKind::NoConnectionPreamble.into());
            }
        }
        None => {
            return Err(ErrorKind::NoConnectionPreamble.into());
        }
    };

    let channels = ChannelConn::setup(framed);

    let metadata = if conn.is_trusted {
        // Relayed through trusted proxy. Proxy authenticates end client and generates
        // preamble so we can trust it. Use identity provided in preamble.
        Some(
            try_convert_preamble_to_metadata(&preamble, conn.pending.addr.ip(), &channels.logger)
                .await?,
        )
    } else {
        None
    };

    handle_wireproto(conn, channels, preamble.reponame, metadata, false)
        .await
        .context("Failed to handle_wireproto")?;

    Ok(())
}

async fn handle_http<S: MononokeStream>(conn: AcceptedConnection, stream: S) -> Result<()> {
    STATS::http_accepted.add_value(1);

    let svc = MononokeHttpService::<S>::new(conn);

    // NOTE: We don't select h2 in alpn, so we only expect HTTP/1.1 here.
    Http::new()
        .http1_only(true)
        .serve_connection(stream, svc)
        .with_upgrades()
        .await
        .context("Failed to serve_connection")?;

    Ok(())
}

pub async fn handle_wireproto(
    conn: AcceptedConnection,
    channels: ChannelConn,
    reponame: String,
    metadata: Option<Metadata>,
    client_debug: bool,
) -> Result<()> {
    let metadata = if let Some(metadata) = metadata {
        metadata
    } else {
        // Most likely client is not trusted. Use TLS connection
        // cert as identity.
        Metadata::new(
            Some(&generate_session_id().to_string()),
            conn.is_trusted,
            (*conn.identities).clone(),
            Priority::Default,
            client_debug,
            Some(conn.pending.addr.ip()),
            None,
        )
        .await
    };

    let metadata = Arc::new(metadata);

    let ChannelConn {
        stdin,
        stdout,
        stderr,
        logger,
        keep_alive,
        join_handle,
    } = channels;

    if metadata.client_debug() {
        info!(&logger, "{:#?}", metadata; "remote" => "true");
    }

    // Don't let the logger hold onto the channel. This is a bit fragile (but at least it breaks
    // tests deterministically).
    drop(logger);

    let stdio = Stdio {
        metadata,
        stdin,
        stdout,
        stderr,
    };

    // Don't immediately return error here, we need to cleanup our
    // handlers like keep alive, otherwise they will run forever.
    let result = request_handler(
        conn.pending.acceptor.fb,
        reponame,
        &conn.pending.acceptor.repo_handlers,
        &conn.pending.acceptor.security_checker,
        stdio,
        conn.pending.acceptor.load_limiter.clone(),
        conn.pending.addr.ip(),
        conn.pending.acceptor.scribe.clone(),
        conn.pending.acceptor.qps.clone(),
    )
    .await
    .context("Failed to execute request_handler");

    // Shutdown our keepalive handler
    keep_alive.abort();

    join_handle
        .await
        .context("Failed to join ChannelConn")?
        .context("Failed to close ChannelConn")?;

    result
}

pub struct FramedConn<R, W> {
    rd: FramedRead<R, SshDecoder>,
    wr: FramedWrite<W, SshEncoder>,
}

impl<R, W> FramedConn<R, W>
where
    R: AsyncRead + Send + std::marker::Unpin + 'static,
    W: AsyncWrite + Send + std::marker::Unpin + 'static,
{
    pub fn setup(rd: R, wr: W) -> Self {
        // NOTE: FramedRead does buffering, so no need to wrap with a BufReader here.
        let rd = FramedRead::new(rd, SshDecoder::new());
        let wr = FramedWrite::new(wr, SshEncoder::new());
        Self { rd, wr }
    }
}

pub struct ChannelConn {
    stdin: BoxStream<Bytes, io::Error>,
    stdout: mpsc::Sender<Bytes>,
    stderr: mpsc::UnboundedSender<Bytes>,
    logger: Logger,
    keep_alive: AbortHandle,
    join_handle: JoinHandle<Result<(), io::Error>>,
}

impl ChannelConn {
    pub fn setup<R, W>(conn: FramedConn<R, W>) -> Self
    where
        R: AsyncRead + Send + std::marker::Unpin + 'static,
        W: AsyncWrite + Send + std::marker::Unpin + 'static,
    {
        let FramedConn { rd, wr } = conn;

        let stdin = Box::new(rd.compat().filter_map(|s| {
            if s.stream() == IoStream::Stdin {
                Some(s.data())
            } else {
                None
            }
        }));

        let (stdout, stderr, keep_alive, join_handle) = {
            let (otx, orx) = mpsc::channel(1);
            let (etx, erx) = mpsc::unbounded();
            let (ktx, krx) = mpsc::unbounded();

            let orx = orx
                .map(|blob| split_bytes_in_chunk(blob, CHUNK_SIZE))
                .flatten()
                .map(|v| SshMsg::new(IoStream::Stdout, v));
            let erx = erx
                .map(|blob| split_bytes_in_chunk(blob, CHUNK_SIZE))
                .flatten()
                .map(|v| SshMsg::new(IoStream::Stderr, v));
            let krx = krx.map(|v| SshMsg::new(IoStream::Stderr, v));

            // Glue them together
            let fwd = orx
                .select(erx)
                .select(krx)
                .compat()
                .map_err(|()| io::Error::new(io::ErrorKind::Other, "huh?"))
                .forward(wr);

            let keep_alive_sender = async move {
                loop {
                    tokio::time::sleep(KEEP_ALIVE_INTERVAL).await;
                    if ktx.unbounded_send(Bytes::new()).is_err() {
                        break;
                    }
                }
            };
            let (keep_alive_sender, keep_alive_abort) =
                futures::future::abortable(keep_alive_sender);

            // spawn a task for sending keepalive messages
            tokio::spawn(keep_alive_sender);

            // spawn a task for forwarding stdout/err into stream
            let join_handle = tokio::spawn(fwd);

            (otx, etx, keep_alive_abort, join_handle)
        };

        let logger = create_conn_logger(stderr.clone(), None, None);

        ChannelConn {
            stdin,
            stdout,
            stderr,
            logger,
            keep_alive,
            join_handle,
        }
    }
}

async fn try_convert_preamble_to_metadata(
    preamble: &Preamble,
    addr: IpAddr,
    conn_log: &Logger,
) -> Result<Metadata> {
    let vars = SshEnvVars::from_map(&preamble.misc);
    let client_ip = match vars.ssh_client {
        Some(ssh_client) => ssh_client
            .split_whitespace()
            .next()
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .unwrap_or(addr),
        None => addr,
    };

    let priority = match Priority::extract_from_preamble(&preamble) {
        Ok(Some(p)) => {
            info!(&conn_log, "Using priority: {}", p; "remote" => "true");
            p
        }
        Ok(None) => Priority::Default,
        Err(e) => {
            warn!(&conn_log, "Could not parse priority: {}", e; "remote" => "true");
            Priority::Default
        }
    };

    let identity = {
        #[cfg(fbcode_build)]
        {
            // SSH Connections are either authentication via ssh certificate principals or
            // via some form of keyboard-interactive. In the case of certificates we should always
            // rely on these. If they are not present, we should fallback to use the unix username
            // as the primary principal.
            let ssh_identities = match vars.ssh_cert_principals {
                Some(ssh_identities) => ssh_identities,
                None => preamble
                    .unix_name()
                    .ok_or_else(|| anyhow!("missing username and principals from preamble"))?
                    .to_string(),
            };

            MononokeIdentity::try_from_ssh_encoded(&ssh_identities)?
        }
        #[cfg(not(fbcode_build))]
        {
            use maplit::btreeset;
            btreeset! { MononokeIdentity::new(
                "USER",
               preamble
                    .unix_name()
                    .ok_or_else(|| anyhow!("missing username from preamble"))?
                    .to_string(),
            )?}
        }
    };

    Ok(Metadata::new(
        preamble.misc.get("session_uuid"),
        true,
        identity,
        priority,
        preamble
            .misc
            .get("client_debug")
            .map(|debug| debug.parse::<bool>().unwrap_or_default())
            .unwrap_or_default(),
        Some(client_ip),
        None,
    )
    .await)
}

// TODO(stash): T33775046 we had to chunk responses because hgcli
// can't cope with big chunks
fn split_bytes_in_chunk<E>(blob: Bytes, chunksize: usize) -> impl Stream<Item = Bytes, Error = E> {
    stream::unfold(blob, move |mut remain| {
        let len = remain.len();
        if len > 0 {
            let ret = remain.split_to(::std::cmp::min(chunksize, len));
            Some(Ok((ret, remain)))
        } else {
            None
        }
    })
}
