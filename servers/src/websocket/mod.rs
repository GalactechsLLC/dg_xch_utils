pub mod farmer;
pub mod harvester;

use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::{
    ChiaMessageHandler, NodeType, PeerMap, SocketPeer, WebsocketConnection, WebsocketMsgStream,
};
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes, AllowAny, SslInfo,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::ChiaProtocolVersion;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_tungstenite::{is_upgrade_request, upgrade, HyperWebsocket};
use hyper_util::rt::TokioIo;
use log::{debug, error};
#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericGauge};
use rustls::{Certificate, PrivateKey, RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::error::TlsError;
use uuid::Uuid;

pub struct WebsocketServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: Option<SslInfo>,
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
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Option<WebSocketMetrics>>,
}
impl WebsocketServer {
    pub fn new(
        config: &WebsocketServerConfig,
        peers: PeerMap,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        #[cfg(feature = "metrics")] metrics: Arc<Option<WebSocketMetrics>>,
    ) -> Result<Self, Error> {
        let (certs, key, root_certs) = if let Some(ssl_info) = &config.ssl_info {
            (
                load_certs(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.certs.private_crt
                ))?,
                load_private_key(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.certs.private_key
                ))?,
                load_certs(&format!(
                    "{}/{}",
                    &ssl_info.root_path, &ssl_info.ca.private_crt
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
            #[cfg(feature = "metrics")]
            metrics,
        })
    }
    pub fn with_ca(
        config: &WebsocketServerConfig,
        peers: PeerMap,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        cert_data: &str,
        key_data: &str,
        #[cfg(feature = "metrics")] metrics: Arc<Option<WebSocketMetrics>>,
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
            #[cfg(feature = "metrics")]
            metrics,
        })
    }

    pub async fn run(&self, run: Arc<AtomicBool>) -> Result<(), Error> {
        let listener = TcpListener::bind(self.socket_address).await?;
        let acceptor = TlsAcceptor::from(self.server_config.clone());
        let mut http = Builder::new();
        http.keep_alive(true);
        while run.load(Ordering::Relaxed) {
            let run = run.clone();
            let peers = self.peers.clone();
            let handlers = self.message_handlers.clone();
            #[cfg(feature = "metrics")]
            let metrics = self.metrics.clone();
            select!(
                res = listener.accept() => {
                    match res {
                        Ok((stream, _)) => {
                            let peers = peers.clone();
                            let message_handlers = handlers.clone();
                            #[cfg(feature = "metrics")]
                            let metrics = metrics.clone();
                            match acceptor.accept(stream).await {
                                Ok(stream) => {
                                    let addr = stream.get_ref().0.peer_addr().ok();
                                    let mut peer_id = None;
                                    if let Some(certs) = stream.get_ref().1.peer_certificates() {
                                        if !certs.is_empty() {
                                            peer_id = Some(Bytes32::new(hash_256(&certs[0].0)));
                                        }
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
                                    let connection = http.serve_connection(TokioIo::new(stream), service).with_upgrades();
                                    tokio::spawn( async move {
                                        if let Err(e) = connection.await {
                                            error!("Error serving connection: {e:?}");
                                        }
                                        Ok::<(), Error>(())
                                    });
                                }
                                Err(e) => {
                                    error!("Error accepting connection: {e:?}");
                                }
                            }
                        }
                        Err(e) => {
                            error!("Error accepting connection: {e:?}");
                        }
                    }
                },
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            );
        }
        Ok(())
    }

    pub fn init(
        certs: Vec<Certificate>,
        key: PrivateKey,
        root_certs: Vec<Certificate>,
    ) -> Result<Arc<ServerConfig>, Error> {
        let mut root_cert_store = RootCertStore::empty();
        for cert in root_certs {
            root_cert_store.add(&cert).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Root Cert for Server: {e:?}"),
                )
            })?;
        }
        Ok(Arc::new(
            ServerConfig::builder()
                .with_safe_defaults()
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
}

fn connection_handler(
    mut data: ConnectionData,
    #[cfg(feature = "metrics")] metrics: Arc<Option<WebSocketMetrics>>,
) -> Result<Response<Full<Bytes>>, tungstenite::error::Error> {
    if is_upgrade_request(&data.req) {
        let (response, websocket) = upgrade(&mut data.req, None)?;
        let addr = data
            .addr
            .ok_or_else(|| Error::new(ErrorKind::Other, "Invalid SocketAddr"))?;
        let peer_id = Arc::new(
            data.peer_id
                .or_else(|| {
                    if let Some(key) = data.req.headers().get("ssl-client-cert") {
                        debug!("Using ssl-client header");
                        Some(Bytes32::new(hash_256(key.as_bytes())))
                    } else if let Some(key) = data.req.headers().get("chia-client-cert") {
                        Some(Bytes32::new(hash_256(key.as_bytes())))
                    } else {
                        error!("Invalid Peer - No Cert or Header");
                        None
                    }
                })
                .ok_or_else(|| {
                    tungstenite::error::Error::Tls(TlsError::Rustls(
                        rustls::Error::NoCertificatesPresented,
                    ))
                })?,
        );
        #[cfg(feature = "metrics")]
        if let Some(metrics) = metrics.as_ref() {
            if let Some(gauge) = metrics.connected_clients.as_ref() {
                gauge.add(1);
            }
        }
        tokio::spawn(async move {
            if let Err(e) = handle_connection(
                addr,
                peer_id,
                websocket,
                data.peers,
                data.message_handlers.clone(),
                data.run.clone(),
            )
            .await
            {
                error!("Error in websocket connection: {e}");
            }
            #[cfg(feature = "metrics")]
            if let Some(metrics) = metrics.as_ref() {
                if let Some(gauge) = metrics.connected_clients.as_ref() {
                    gauge.sub(1);
                }
            }
        });
        Ok(response)
    } else {
        Ok(Response::new(Full::new(Bytes::from(
            "HTTP NOT SUPPORTED ON THIS ENDPOINT",
        ))))
    }
}

async fn handle_connection(
    _peer_addr: SocketAddr,
    peer_id: Arc<Bytes32>,
    websocket: HyperWebsocket,
    peers: PeerMap,
    message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    run: Arc<AtomicBool>,
) -> Result<(), tungstenite::error::Error> {
    let (websocket, mut stream) = WebsocketConnection::new(
        WebsocketMsgStream::TokioIo(websocket.await?),
        message_handlers,
        peer_id.clone(),
        peers.clone(),
    );
    let removed = peers.write().await.insert(
        *peer_id,
        Arc::new(SocketPeer {
            node_type: Arc::new(RwLock::new(NodeType::Unknown)),
            protocol_version: Arc::new(RwLock::new(ChiaProtocolVersion::default())),
            websocket: Arc::new(RwLock::new(websocket)),
        }),
    );
    if let Some(removed) = removed {
        debug!("Sending Close to Peer");
        let _ = removed.websocket.write().await.close(None).await;
    }
    stream.run(run).await;
    Ok(())
}
