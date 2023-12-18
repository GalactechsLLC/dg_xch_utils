pub mod farmer;
pub mod harvester;

use crate::rpc::RpcHandler;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_core::protocols::{
    ChiaMessageHandler, NodeType, PeerMap, SocketPeer, WebsocketConnection, WebsocketMsgStream,
};
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes, AllowAny, SslInfo, CHIA_CA_CRT, CHIA_CA_KEY,
};
use dg_xch_serialize::hash_256;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1::Builder;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_tungstenite::{is_upgrade_request, upgrade, HyperWebsocket};
use hyper_util::rt::TokioIo;
use log::{debug, error, info};
use rustls::{RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::env;
use std::io::{Error, ErrorKind};
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::Mutex;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite;
use uuid::Uuid;

pub struct WebsocketServerConfig {
    pub host: String,
    pub port: u16,
    pub ssl_info: Option<SslInfo>,
}

pub struct WebsocketServer<T: Send + Sync + 'static, R: RpcHandler<T> + Send + Sync + 'static> {
    pub socket_address: SocketAddr,
    pub server_config: Arc<ServerConfig>,
    pub peers: PeerMap,
    pub message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    pub rpc_handler: Arc<R>,
    pub shared_state: Arc<T>,
}
impl<T: Send + Sync + 'static, R: RpcHandler<T> + Send + Sync + 'static> WebsocketServer<T, R> {
    pub fn new(
        config: &WebsocketServerConfig,
        peers: PeerMap,
        message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        rpc_handler: Arc<R>,
        shared_state: Arc<T>,
    ) -> Result<Self, Error> {
        let server_config = Self::init(config)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("Invalid Cert: {:?}", e)))?;
        let socket_address = Self::init_socket(config)?;
        Ok(WebsocketServer {
            socket_address,
            server_config,
            peers,
            message_handlers,
            rpc_handler,
            shared_state,
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
            let rpc_handler = self.rpc_handler.clone();
            let shared_state = self.shared_state.clone();
            select!(
                res = listener.accept() => {
                    match res {
                        Ok((stream, _)) => {
                            info!("New Client Connection");
                            let peers = peers.clone();
                            let message_handlers = handlers.clone();
                            let rpc_handler = rpc_handler.clone();
                            let shared_state = shared_state.clone();
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
                                    run: run.clone(),
                                    rpc_handler: rpc_handler.clone(),
                                    shared_state: shared_state.clone()
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
        } else if let (Some(crt), Some(key)) = (
            env::var("PRIVATE_CA_CRT").ok(),
            env::var("PRIVATE_CA_KEY").ok(),
        ) {
            let (cert_bytes, key_bytes) = generate_ca_signed_cert_data(&crt, &key)?;
            (
                load_certs_from_bytes(cert_bytes.as_bytes())?,
                load_private_key_from_bytes(key_bytes.as_bytes())?,
                load_certs_from_bytes(crt.as_bytes())?,
            )
        } else {
            let (cert_bytes, key_bytes) = generate_ca_signed_cert_data(CHIA_CA_CRT, CHIA_CA_KEY)?;
            (
                load_certs_from_bytes(cert_bytes.as_bytes())?,
                load_private_key_from_bytes(key_bytes.as_bytes())?,
                load_certs_from_bytes(CHIA_CA_CRT.as_bytes())?,
            )
        };
        let mut root_cert_store = RootCertStore::empty();
        for cert in root_certs {
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

struct ConnectionData<T: Send + Sync + 'static, R: RpcHandler<T> + Send + Sync + 'static> {
    pub addr: Option<SocketAddr>,
    pub peer_id: Arc<Option<Bytes32>>,
    pub req: Request<Incoming>,
    pub peers: PeerMap,
    pub message_handlers: Arc<Mutex<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
    pub run: Arc<AtomicBool>,
    pub rpc_handler: Arc<R>,
    pub shared_state: Arc<T>,
}

async fn connection_handler<T: Send + Sync + 'static, R: RpcHandler<T> + Send + Sync + 'static>(
    mut data: ConnectionData<T, R>,
) -> Result<Response<Full<Bytes>>, tungstenite::error::Error> {
    if is_upgrade_request(&data.req) {
        let (response, websocket) = upgrade(&mut data.req, None)?;
        let addr = data
            .addr
            .ok_or_else(|| Error::new(ErrorKind::Other, "Invalid SocketAddr"))?;
        let peer_id = Arc::new(
            data.peer_id
                .ok_or_else(|| Error::new(ErrorKind::Other, "Invalid Peer"))?,
        );
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
                error!("Error in websocket connection: {}", e);
            }
        });
        Ok(response)
    } else {
        data.rpc_handler
            .as_ref()
            .handle(data.req, data.shared_state.clone())
            .await
            .map_err(tungstenite::error::Error::Io)
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
        peers.clone(),
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
