pub mod farmer;
pub mod harvester;

use std::collections::HashMap;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_core::protocols::{ChiaMessageHandler, NodeType, PeerMap, SocketPeer, WebsocketConnection, WebsocketMsgStream};
use dg_xch_core::ssl::{load_certs, load_private_key, AllowAny};
use dg_xch_serialize::hash_256;
use http_body_util::Full;
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper_tungstenite::{is_upgrade_request, upgrade, HyperWebsocket};
use hyper_util::rt::TokioIo;
use log::{debug, error, info};
use rustls::{RootCertStore, ServerConfig};
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use hyper::body::{Bytes, Incoming};
use hyper::{Request, Response};
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{tungstenite};
use uuid::Uuid;

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslCertInfo {
    #[serde(default)]
    pub public_crt: Option<String>,
    #[serde(default)]
    pub public_key: Option<String>,
    pub private_crt: String,
    pub private_key: String,
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslInfo {
    pub root_path: String,
    pub certs: SslCertInfo,
    pub ca: SslCertInfo,
}

pub struct WebsocketServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: SslInfo,
}

pub struct WebsocketServer {
    pub socket_address: SocketAddr,
    pub server_config: Arc<ServerConfig>,
    pub peers: PeerMap,
    pub message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
}
impl WebsocketServer {
    pub fn new(config: &WebsocketServerConfig, peers: PeerMap, message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>) -> Result<Self, Error> {
        let server_config = Self::init(config).map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Invalid Cert for Farmer Server: {:?}", e),
            )
        })?;
        let socket_address = Self::init_socket(config)?;
        Ok(WebsocketServer {
            socket_address,
            server_config,
            peers,
            message_handlers
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
            select!(
                res = listener.accept() => {
                    match res {
                        Ok((stream, _)) => {
                            info!("New Client Connection");
                            let peers = peers.clone();
                            let message_handlers = handlers.clone();
                            let stream = acceptor.accept(stream).await?;
                            let addr = stream.get_ref().0.peer_addr().ok();
                            let mut peer_id = None;
                            if let Some(certs) = stream.get_ref().1.peer_certificates() {
                                if !certs.is_empty() {
                                    peer_id = Some(Bytes32::new(&hash_256(&certs[0].0)));
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
                                    run: run.clone()
                                };
                                connection_handler(data)
                            });
                            let connection = http.serve_connection(TokioIo::new(stream), service).with_upgrades();
                            tokio::spawn( async move {
                                if let Err(err) = connection.await {
                                    println!("Error serving connection: {:?}", err);
                                }
                                Ok::<(), Error>(())
                            });
                        }
                        Err(e) => {
                            error!("Error accepting connection: {:?}", e);
                        }
                    }
                },
                _ = tokio::time::sleep(Duration::from_millis(10)) => {}
            )
        }
        Ok(())
    }

    pub fn init(config: &WebsocketServerConfig) -> Result<Arc<ServerConfig>, Error> {
        let certs = load_certs(&format!(
            "{}/{}",
            &config.ssl_info.root_path, &config.ssl_info.certs.private_crt
        ))?;
        let key = load_private_key(&format!(
            "{}/{}",
            &config.ssl_info.root_path, &config.ssl_info.certs.private_key
        ))?;
        let mut root_cert_store = RootCertStore::empty();
        for cert in load_certs(&format!(
            "{}/{}",
            &config.ssl_info.root_path, &config.ssl_info.ca.private_crt
        ))? {
            root_cert_store.add(&cert).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Invalid Root Cert for Server: {:?}", e),

                )
            })?;
        }
        Ok(Arc::new(
            ServerConfig::builder()
                .with_safe_defaults()
                .with_client_cert_verifier(AllowAny::new(root_cert_store))
                .with_single_cert(certs, key)
                .map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("Invalid Cert for Server: {:?}", e),
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
                    format!("Failed to parse Host: {:?}", e),
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
    pub message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    pub run: Arc<AtomicBool>,
}

async fn connection_handler(
    mut data: ConnectionData
) -> Result<Response<Full<Bytes>>, tungstenite::error::Error> {
    if is_upgrade_request(&data.req) {
        let (response, websocket) = upgrade(&mut data.req, None)?;
        let addr = data.addr.ok_or_else(|| Error::new(ErrorKind::Other, "Invalid SocketAddr"))?;
        let peer_id =
            Arc::new(data.peer_id.ok_or_else(|| Error::new(ErrorKind::Other, "Invalid Peer"))?);
        tokio::spawn(async move {
            if let Err(e) =
                handle_connection(addr, peer_id, websocket, data.peers, data.message_handlers.clone(), data.run.clone())
                    .await
            {
                error!("Error in websocket connection: {}", e);
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
    message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    run: Arc<AtomicBool>,
) -> Result<(), tungstenite::error::Error> {
    let (websocket, mut stream) = WebsocketConnection::new(
        WebsocketMsgStream::TokioIo(websocket.await?),
        message_handlers,
        peer_id.clone(),
        peers.clone()
    );
    let removed = peers.lock().await.insert(
        *peer_id,
        Arc::new(SocketPeer {
            node_type: Arc::new(Mutex::new(NodeType::Unknown)),
            websocket: Arc::new(Mutex::new(websocket)),
        }),
    );
    if let Some(removed) = removed {
        debug!("Sending Close to Peer");
        let _ = removed.websocket.lock().await.close(None).await;
    }
    let _ = stream.run(run).await;
    Ok(())
}