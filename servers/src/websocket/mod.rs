pub mod farmer;
pub mod harvester;

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::ban::BanRegistry;
use dg_xch_core::protocols::outbound_limiter::OutboundLimiter;
use dg_xch_core::protocols::rate_limits::RateLimiter;
use dg_xch_core::protocols::{
    ChiaMessageHandler, NodeType, PeerMap, SocketPeer, WebsocketConnection, WebsocketMsgStream,
};
use dg_xch_core::ssl::{
    AllowAny, SslInfo, generate_ca_signed_cert_data, load_certs, load_certs_from_bytes,
    load_private_key, load_private_key_from_bytes,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::ChiaProtocolVersion;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_tungstenite::{is_upgrade_request, upgrade};
use hyper_util::rt::TokioIo;
use log::{debug, error, info, warn};
#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericGauge};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::{RwLock, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::error::TlsError;
use uuid::Uuid;

pub struct WebsocketServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: Option<SslInfo>,
}

// rustls reports a peer that drops the socket without sending TLS close_notify as an error, and
// public peers do this constantly — routine churn, not a server fault. That one class is demoted
// to a debounced WARN (DEBUG carries every instance); every other connection error stays ERROR.
fn log_connection_error(context: &str, rendered: &str) {
    if rendered.contains("close_notify") {
        static LAST_WARN_UNIX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        const DEBOUNCE_SECS: u64 = 600;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let last = LAST_WARN_UNIX.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= DEBOUNCE_SECS
            && LAST_WARN_UNIX
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            warn!("{context} (debounced; routine peer close without TLS close_notify): {rendered}");
        } else {
            debug!("{context}: {rendered}");
        }
    } else {
        error!("{context}: {rendered}");
    }
}

#[cfg(feature = "metrics")]
pub struct WebSocketMetrics {
    pub connected_clients: Arc<Option<GenericGauge<AtomicU64>>>,
}

pub struct WebsocketServer {
    pub socket_address: SocketAddr,
    pub server_config: Arc<ServerConfig>,
    pub peers: PeerMap,
    pub message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    /// Install the per-connection inbound rate limiter on every accepted connection. Off by
    /// default; the full-node server turns it on. Farmer/harvester servers leave it off.
    pub rate_limited: bool,
    /// The server-wide timed ban list. The accept path refuses a
    /// host that is banned and not yet expired; the read loop and the message handlers enter a
    /// misbehaving peer's host here (via the registry injected into each [`SocketPeer`]). One
    /// registry per server instance, shared across all its connections.
    pub bans: Arc<BanRegistry>,
    pub inbound_limit: Arc<Semaphore>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Option<WebSocketMetrics>>,
}
impl WebsocketServer {
    pub fn new(
        config: &WebsocketServerConfig,
        peers: PeerMap,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    ) -> Result<Self, Error> {
        let (certs, key, root_certs) = if let Some(ssl_info) = &config.ssl_info {
            (
                load_certs(&format!(
                    "{}/{}",
                    ssl_info.root_path, ssl_info.certs.private_crt
                ))?,
                load_private_key(&format!(
                    "{}/{}",
                    ssl_info.root_path, ssl_info.certs.private_key
                ))?,
                load_certs(&format!(
                    "{}/{}",
                    ssl_info.root_path, ssl_info.ca.private_crt
                ))?,
            )
        } else {
            let (cert_bytes, key_bytes) =
                generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())?;
            (
                load_certs_from_bytes(&cert_bytes)?,
                load_private_key_from_bytes(&key_bytes)?,
                load_certs_from_bytes(CHIA_CA_CRT.as_bytes())?,
            )
        };
        let server_config = Self::init(certs, key, root_certs)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid Cert: {e:?}")))?;
        let socket_address = Self::init_socket(config)?;
        Ok(WebsocketServer {
            socket_address,
            server_config,
            peers,
            message_handlers,
            rate_limited: false,
            bans: Arc::new(BanRegistry::default()),
            inbound_limit: Arc::new(Semaphore::new(128)),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(None),
        })
    }

    #[cfg(feature = "metrics")]
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<Option<WebSocketMetrics>>) -> Self {
        self.metrics = metrics;
        self
    }

    pub fn with_ca(
        config: &WebsocketServerConfig,
        peers: PeerMap,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        cert_data: &str,
        key_data: &str,
    ) -> Result<Self, Error> {
        let (cert_bytes, key_bytes) =
            generate_ca_signed_cert_data(cert_data.as_bytes(), key_data.as_bytes())?;
        let (certs, key, root_certs) = (
            load_certs_from_bytes(&cert_bytes)?,
            load_private_key_from_bytes(&key_bytes)?,
            load_certs_from_bytes(cert_data.as_bytes())?,
        );
        let server_config = Self::init(certs, key, root_certs)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid Cert: {e:?}")))?;
        let socket_address = Self::init_socket(config)?;
        Ok(WebsocketServer {
            socket_address,
            server_config,
            peers,
            message_handlers,
            rate_limited: false,
            bans: Arc::new(BanRegistry::default()),
            inbound_limit: Arc::new(Semaphore::new(128)),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(None),
        })
    }

    pub async fn run(&self, run: Arc<AtomicBool>) -> Result<(), Error> {
        let listener = TcpListener::bind(self.socket_address).await?;
        let acceptor = TlsAcceptor::from(self.server_config.clone());
        let handshakes = Arc::new(Semaphore::new(64));
        let mut connections = JoinSet::new();
        while run.load(Ordering::Relaxed) {
            let run = run.clone();
            let peers = self.peers.clone();
            let handlers = self.message_handlers.clone();
            let bans = self.bans.clone();
            #[cfg(feature = "metrics")]
            let metrics = self.metrics.clone();
            select!(
                res = listener.accept() => {
                    match res {
                        Ok((stream, address)) => {
                            if bans.is_banned(&address.ip()) {
                                continue;
                            }
                            let Ok(permit) = handshakes.clone().try_acquire_owned() else {
                                continue;
                            };
                            let acceptor = acceptor.clone();
                            let peers = peers.clone();
                            let message_handlers = handlers.clone();
                            let bans = bans.clone();
                            #[cfg(feature = "metrics")]
                            let metrics = metrics.clone();
                            let rate_limited = self.rate_limited;
                            let inbound_limit = self.inbound_limit.clone();
                            connections.spawn(async move {
                            let _permit = permit;
                            match tokio::time::timeout(Duration::from_secs(5), acceptor.accept(stream)).await {
                                Ok(Ok(stream)) => {
                                    let addr = stream.get_ref().0.peer_addr().ok();
                                    let mut peer_id = None;
                                    if let Some(certs) = stream.get_ref().1.peer_certificates()
                                        && !certs.is_empty() {
                                            peer_id = Some(Bytes32::new(hash_256(&certs[0])));
                                    }
                                    let peer_id = Arc::new(peer_id);
                                    let service = service_fn(move |req| {
                                        let data = ConnectionData {
                                            addr,
                                            peer_id: peer_id.clone(),
                                            req,
                                            peers: peers.clone(),
                                            message_handlers: message_handlers.clone(),
                                            run: run.clone(),
                                            rate_limited,
                                            bans: bans.clone(),
                                            inbound_limit: inbound_limit.clone(),
                                        };
                                        #[cfg(feature = "metrics")]
                                        let metrics = metrics.clone();
                                        async move {
                                            connection_handler(
                                                data,
                                                 #[cfg(feature = "metrics")]
                                                metrics.clone()
                                            )
                                        }
                                    });
                                    let http = Builder::new();
                                    let connection = http.serve_connection(TokioIo::new(stream), service).with_upgrades();
                                    if let Ok(Err(error)) = tokio::time::timeout(Duration::from_secs(15), connection).await {
                                        log_connection_error("Error serving connection", &format!("{error:?}"));
                                    }
                                }
                                Ok(Err(e)) => {
                                    log_connection_error("Error accepting connection", &format!("{e:?}"));
                                }
                                Err(_) => debug!("Peer TLS handshake timed out"),
                            }
                            });
                        }
                        Err(e) => {
                            log_connection_error("Error accepting connection", &format!("{e:?}"));
                        }
                    }
                },
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        log_connection_error("Connection task failed", &error.to_string());
                    }
                },
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            );
        }
        connections.shutdown().await;
        Ok(())
    }

    /// Run the Chia peer session over a websocket upgraded by an embedding server.
    pub async fn handle_stream(
        &self,
        peer_addr: SocketAddr,
        peer_id: Bytes32,
        websocket: WebsocketMsgStream,
        run: Arc<AtomicBool>,
    ) -> Result<(), tungstenite::error::Error> {
        if self.bans.is_banned(&peer_addr.ip()) {
            return Err(tungstenite::error::Error::ConnectionClosed);
        }
        let _permit = self
            .inbound_limit
            .clone()
            .try_acquire_owned()
            .map_err(|_| tungstenite::error::Error::ConnectionClosed)?;
        handle_connection(
            peer_addr,
            Arc::new(peer_id),
            websocket,
            self.peers.clone(),
            self.message_handlers.clone(),
            run,
            self.rate_limited,
            self.bans.clone(),
        )
        .await
    }

    pub fn init(
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        root_certs: Vec<CertificateDer<'static>>,
    ) -> Result<Arc<ServerConfig>, Error> {
        let mut root_cert_store = RootCertStore::empty();
        for cert in root_certs {
            root_cert_store.add(cert).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Root Cert for Server: {e:?}"),
                )
            })?;
        }
        Ok(Arc::new(
            // TLS below 1.3 is refused on every server-side socket. rustls' default builder
            // would accept 1.2 as well, so the version set must be pinned explicitly or the
            // floor is silently lost (`servers/tests/tls13_floor.rs`).
            ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .with_client_cert_verifier(AllowAny::new())
                .with_single_cert(certs, key)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("Invalid Cert for Server: {e:?}"),
                    )
                })?,
        ))
    }

    pub fn init_socket(config: &WebsocketServerConfig) -> Result<SocketAddr, Error> {
        Ok(SocketAddr::from((
            Ipv4Addr::from_str(if config.host == "localhost" {
                "127.0.0.1"
            } else {
                &config.host
            })
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to parse Host: {e:?}"),
                )
            })?,
            config.port,
        )))
    }
}

struct ConnectionData {
    pub addr: Option<SocketAddr>,
    pub peer_id: Arc<Option<Bytes32>>,
    pub req: Request<Incoming>,
    pub peers: PeerMap,
    pub message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    pub run: Arc<AtomicBool>,
    pub rate_limited: bool,
    pub bans: Arc<BanRegistry>,
    pub inbound_limit: Arc<Semaphore>,
}

fn connection_handler(
    mut data: ConnectionData,
    #[cfg(feature = "metrics")] metrics: Arc<Option<WebSocketMetrics>>,
) -> Result<Response<Full<Bytes>>, Error> {
    if is_upgrade_request(&data.req) {
        let permit = match data.inbound_limit.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                return Response::builder()
                    .status(503)
                    .body(Full::new(Bytes::from("Peer capacity reached")))
                    .map_err(Error::other);
            }
        };
        // Refuse a host that is banned and not yet expired, BEFORE completing the websocket
        // upgrade. Returns HTTP 403 so the dialing client's handshake fails fast; a banned
        // spammer that closes and immediately reconnects is turned away for the rest of its
        // ban window.
        if let Some(addr) = data.addr
            && data.bans.is_banned(&addr.ip())
        {
            warn!(
                "Refusing banned host {}: still within ban window",
                addr.ip()
            );
            return Ok(Response::builder()
                .status(403)
                .body(Full::new(Bytes::from("Peer is banned")))
                .unwrap_or_else(|_| Response::new(Full::new(Bytes::from("Peer is banned")))));
        }
        // A batch of full blocks (or a weight proof) exceeds tungstenite's default 16 MiB
        // per-frame cap and would be rejected as `MessageTooLong`.
        let ws_config = hyper_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(64 << 20))
            .max_frame_size(Some(64 << 20));
        let (response, websocket) =
            upgrade(&mut data.req, Some(ws_config)).map_err(Error::other)?;
        let addr = data
            .addr
            .ok_or_else(|| Error::other("Invalid SocketAddr"))?;
        let peer_id = Arc::new(
            data.peer_id
                .ok_or_else(|| {
                    tungstenite::error::Error::Tls(TlsError::Rustls(Box::new(
                        rustls::Error::NoCertificatesPresented,
                    )))
                })
                .map_err(Error::other)?,
        );
        #[cfg(feature = "metrics")]
        if let Some(metrics) = metrics.as_ref()
            && let Some(gauge) = metrics.connected_clients.as_ref()
        {
            gauge.add(1);
        }
        tokio::spawn(async move {
            let _permit = permit;
            let websocket = match websocket.await {
                Ok(websocket) => WebsocketMsgStream::TokioIo(Box::new(websocket)),
                Err(error) => {
                    log_connection_error(
                        "Error upgrading websocket connection",
                        &error.to_string(),
                    );
                    return;
                }
            };
            if let Err(e) = handle_connection(
                addr,
                peer_id,
                websocket,
                data.peers,
                data.message_handlers.clone(),
                data.run.clone(),
                data.rate_limited,
                data.bans,
            )
            .await
            {
                log_connection_error("Error in websocket connection", &format!("{e}"));
            }
            #[cfg(feature = "metrics")]
            if let Some(metrics) = metrics.as_ref()
                && let Some(gauge) = metrics.connected_clients.as_ref()
            {
                gauge.sub(1);
            }
        });
        Ok(response)
    } else {
        Ok(Response::new(Full::new(Bytes::from(
            "HTTP NOT SUPPORTED ON THIS ENDPOINT",
        ))))
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    peer_addr: SocketAddr,
    peer_id: Arc<Bytes32>,
    websocket: WebsocketMsgStream,
    peers: PeerMap,
    message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    run: Arc<AtomicBool>,
    rate_limited: bool,
    bans: Arc<BanRegistry>,
) -> Result<(), tungstenite::error::Error> {
    let connected_at = std::time::Instant::now();
    // A full-node listener (the server sets `rate_limited`) installs a fresh per-connection
    // inbound limiter; other server roles leave it off.
    let limiter = if rate_limited {
        Some(Arc::new(RateLimiter::new(true)))
    } else {
        None
    };
    // The send-side companion: paces frequency-capped messages WE push to this inbound peer
    // (farmer/timelord/wallet greetings, re-gossip) against its budget.
    let outbound_limiter = if rate_limited {
        Some(Arc::new(OutboundLimiter::new()))
    } else {
        None
    };
    let (websocket, mut stream) = WebsocketConnection::new(
        websocket,
        message_handlers,
        peer_id.clone(),
        peers.clone(),
        limiter,
        Arc::<str>::from(format!("inbound endpoint={peer_addr}")),
    );
    // Hold our own handle so the teardown below can prove the map still points at THIS
    // connection (and not a peer that reconnected in the meantime) before removing it.
    let v3 = websocket.v3();
    let socket_peer = Arc::new(SocketPeer {
        peer_peak: Arc::new(dg_xch_core::protocols::peer_peak::PeerPeak::default()),
        node_type: Arc::new(RwLock::new(NodeType::Unknown)),
        protocol_version: Arc::new(RwLock::new(ChiaProtocolVersion::default())),
        capabilities: Arc::new(RwLock::new(Vec::new())),
        websocket: Arc::new(RwLock::new(websocket)),
        // The peer's REMOTE host is the ban key, and the server's shared registry is injected so
        // a rate-limit/consensus close on this connection enters this host into the list the
        // accept path consults.
        host: Some(peer_addr.ip()),
        bans: Some(bans),
        outbound_limiter,
        v3,
    });
    let removed = peers.write().await.insert(*peer_id, socket_peer.clone());
    if let Some(removed) = removed {
        debug!("Sending Close to Peer");
        let _ = removed.websocket.write().await.close(None).await;
    }
    stream.run(run).await;
    info!(
        "inbound peer disconnected endpoint={} peer_id={} lifetime_ms={}",
        peer_addr,
        peer_id,
        connected_at.elapsed().as_millis()
    );
    // The read loop returned: the connection is dead. Release this peer's slot so the shared
    // inbound PeerMap stays bounded to LIVE connections. Guard on `Arc` identity so a peer that
    // reconnected (replacing the map value) keeps its fresh entry.
    deregister_peer(&peers, peer_id.as_ref(), &socket_peer).await;
    Ok(())
}

/// Remove a peer's entry from the shared connection map **only if it is still the exact handle
/// we inserted** — comparing by `Arc` pointer identity so a peer that has since reconnected (its
/// map value replaced by a fresh handle) keeps its live entry. Returns whether an entry was
/// removed. This is the teardown half of the insert in [`handle_connection`]; it is what keeps
/// the inbound `PeerMap` flat under connection churn (every collection bounded).
pub(crate) async fn deregister_peer<V>(
    peers: &Arc<RwLock<HashMap<Bytes32, Arc<V>>>>,
    peer_id: &Bytes32,
    ours: &Arc<V>,
) -> bool {
    let mut guard = peers.write().await;
    if guard
        .get(peer_id)
        .is_some_and(|current| Arc::ptr_eq(current, ours))
    {
        guard.remove(peer_id);
        true
    } else {
        false
    }
}

#[cfg(test)]
#[path = "../../tests/unit/websocket/mod/peer_map_tests.rs"]
mod peer_map_tests;
