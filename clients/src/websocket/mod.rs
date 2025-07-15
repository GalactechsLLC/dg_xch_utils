pub mod farmer;
pub mod full_node;
pub mod harvester;
pub mod introducer;
pub mod wallet;

use crate::ClientSSLConfig;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::shared::{Handshake, NoCertificateVerification, CAPABILITIES};
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, SocketPeer,
    WebsocketConnection,
};
use dg_xch_core::protocols::{PeerMap, ProtocolMessageTypes, WebsocketMsgStream};
use dg_xch_core::ssl::{
    generate_ca_signed_cert_data, load_certs, load_certs_from_bytes, load_private_key,
    load_private_key_from_bytes,
};
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use log::debug;
use reqwest::header::{HeaderName, HeaderValue};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ClientConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::{env, fs};
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async_tls_with_config, Connector};
use urlencoding::encode;
use uuid::Uuid;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

pub struct WsClient {
    pub connection: Arc<RwLock<WebsocketConnection>>,
    pub client_config: Arc<WsClientConfig>,
    pub handshake: Option<Handshake>,
    handle: JoinHandle<()>,
    run: Arc<AtomicBool>,
}
impl WsClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        node_type: NodeType,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        run: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        if let Some(ssl_info) = &client_config.ssl_info {
            Self::build(
                client_config.clone(),
                node_type,
                message_handlers,
                run,
                load_certs(&ssl_info.ssl_crt_path)?,
                load_private_key(&ssl_info.ssl_key_path)?,
                &fs::read(&ssl_info.ssl_crt_path)?,
            )
            .await
        } else if let (Some(crt), Some(key)) = (
            env::var("PRIVATE_CA_CRT").ok(),
            env::var("PRIVATE_CA_KEY").ok(),
        ) {
            let (cert_bytes, key_bytes) =
                generate_ca_signed_cert_data(crt.as_bytes(), key.as_bytes())
                    .map_err(|e| Error::other(format!("OpenSSL Errors: {e:?}")))?;
            Self::build(
                client_config,
                node_type,
                message_handlers,
                run,
                load_certs_from_bytes(&cert_bytes)?,
                load_private_key_from_bytes(&key_bytes)?,
                &cert_bytes,
            )
            .await
        } else {
            let (cert_bytes, key_bytes) =
                generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
                    .map_err(|e| Error::other(format!("OpenSSL Errors: {e:?}")))?;
            Self::build(
                client_config,
                node_type,
                message_handlers,
                run,
                load_certs_from_bytes(&cert_bytes)?,
                load_private_key_from_bytes(&key_bytes)?,
                &cert_bytes,
            )
            .await
        }
    }
    pub async fn with_ca(
        client_config: Arc<WsClientConfig>,
        node_type: NodeType,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        run: Arc<AtomicBool>,
        cert_data: &[u8],
        key_data: &[u8],
    ) -> Result<Self, Error> {
        let (cert_bytes, key_bytes) = generate_ca_signed_cert_data(cert_data, key_data)
            .map_err(|e| Error::other(format!("OpenSSL Errors: {e:?}")))?;
        Self::build(
            client_config,
            node_type,
            message_handlers,
            run,
            load_certs_from_bytes(&cert_bytes)?,
            load_private_key_from_bytes(&key_bytes)?,
            &cert_bytes,
        )
        .await
    }

    async fn build(
        client_config: Arc<WsClientConfig>,
        node_type: NodeType,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        run: Arc<AtomicBool>,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        cert_str: &[u8],
    ) -> Result<Self, Error> {
        let mut request = format!("wss://{}:{}/ws", client_config.host, client_config.port)
            .into_client_request()
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to Parse Request: {e}"),
                )
            })?;
        if let Some(m) = &client_config.additional_headers {
            for (k, v) in m {
                request.headers_mut().insert(
                    HeaderName::from_str(k).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!("Failed to Parse Header Name {k},\r\n {e}"),
                        )
                    })?,
                    HeaderValue::from_str(v).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!("Failed to Parse Header value {k},\r\n {e}"),
                        )
                    })?,
                );
            }
        }
        request.headers_mut().insert(
            "chia-client-cert",
            HeaderValue::from_str(&encode(&String::from_utf8_lossy(cert_str))).map_err(|e| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to Parse Header value CHIA_CA_CRT,\r\n {e}"),
                )
            })?,
        );
        let peer_id = Arc::new(Bytes32::new(hash_256(certs[0].as_ref())));
        let (stream, _) = connect_async_tls_with_config(
            request,
            None,
            false,
            Some(Connector::Rustls(Arc::new(
                ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
                    .with_client_auth_cert(certs, key)
                    .map_err(|e| Error::other(format!("Error Building Client: {e:?}")))?,
            ))),
        )
        .await
        .map_err(|e| Error::other(format!("Error Connecting Client: {e:?}")))?;
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let (ws_con, mut stream) = WebsocketConnection::new(
            WebsocketMsgStream::Tls(Box::new(stream)),
            message_handlers,
            peer_id.clone(),
            peers.clone(),
        );
        let connection = Arc::new(RwLock::new(ws_con));
        peers.write().await.insert(
            *peer_id.as_ref(),
            Arc::new(SocketPeer {
                node_type: Arc::new(RwLock::new(NodeType::Harvester)),
                protocol_version: Arc::new(RwLock::new(ChiaProtocolVersion::default())),
                websocket: connection.clone(),
            }),
        );
        let handle_run = run.clone();
        let protocol_version = client_config.protocol_version;
        let mut ws_client = WsClient {
            connection,
            client_config,
            handshake: None,
            handle: tokio::spawn(async move { stream.run(handle_run).await }),
            run,
        };
        ws_client
            .perform_handshake(node_type, protocol_version)
            .await?;
        Ok(ws_client)
    }

    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.run.store(false, Ordering::Relaxed);
        self.connection.write().await.shutdown().await
    }

    pub async fn join(self) -> Result<(), Error> {
        self.handle
            .await
            .map_err(|e| Error::other(format!("Failed to join farmer: {e:?}")))
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.handle.is_finished()
    }

    async fn perform_handshake(
        &mut self,
        node_type: NodeType,
        chia_protocol_version: ChiaProtocolVersion,
    ) -> Result<(), Error> {
        let handshake = oneshot::<Handshake>(
            self.connection.clone(),
            ChiaMessage::new(
                ProtocolMessageTypes::Handshake,
                chia_protocol_version,
                &Handshake {
                    network_id: self.client_config.network_id.to_string(),
                    protocol_version: chia_protocol_version.to_string(),
                    software_version: version(),
                    server_port: self.client_config.port,
                    node_type: node_type as u8,
                    capabilities: CAPABILITIES
                        .iter()
                        .map(|e| (e.0, e.1.to_string()))
                        .collect(),
                },
                None,
            ),
            Some(ProtocolMessageTypes::Handshake),
            chia_protocol_version,
            None,
            Some(15000),
        )
        .await?;
        self.handshake = Some(handshake);
        Ok(())
    }
}

pub struct WsClientConfig {
    pub host: String,
    pub port: u16,
    pub network_id: String,
    pub ssl_info: Option<ClientSSLConfig>,
    //Used to control software version sent to server, default is dg_xch_clients: VERSION
    pub software_version: Option<String>,
    pub protocol_version: ChiaProtocolVersion,
    pub additional_headers: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandshakeResp {
    pub handshake: Handshake,
    pub success: bool,
}

pub struct OneShotHandler {
    pub id: Uuid,
    channel: Sender<Vec<u8>>,
}
#[async_trait]
impl MessageHandler for OneShotHandler {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), Error> {
        debug!("{:?}", msg.as_ref());
        let _ = &self.channel.send(msg.data.clone()).await;
        Ok(())
    }
}

pub async fn oneshot<R: ChiaSerialize>(
    connection: Arc<RwLock<WebsocketConnection>>,
    msg: ChiaMessage,
    resp_type: Option<ProtocolMessageTypes>,
    protocol_version: ChiaProtocolVersion,
    msg_id: Option<u16>,
    timeout: Option<u64>,
) -> Result<R, Error> {
    let handle_uuid = Uuid::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
    let handle = OneShotHandler {
        id: handle_uuid,
        channel: tx,
    };
    let handle = Arc::new(handle);
    let chia_handle = Arc::new(ChiaMessageHandler {
        filter: Arc::new(ChiaMessageFilter {
            msg_type: resp_type,
            id: msg_id,
            custom_fn: None,
        }),
        handle: handle.clone(),
    });
    connection
        .write()
        .await
        .subscribe(handle.id, chia_handle)
        .await;
    let res_handle = tokio::spawn(async move {
        let res = rx.recv().await;
        rx.close();
        res
    });
    connection
        .write()
        .await
        .send(msg.into())
        .await
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to parse send data: {e:?}"),
            )
        })?;
    select!(
        () = tokio::time::sleep(Duration::from_millis(timeout.unwrap_or(15000))) => {
            connection.write().await.unsubscribe(handle.id).await;
            Err(Error::other(
                "Timeout before oneshot completed",
            ))
        }
        res = res_handle => {
            let res = res?;
            if let Some(v) = res {
                let mut cursor = Cursor::new(v);
                connection.read().await.unsubscribe(handle.id).await;
                R::from_bytes(&mut cursor, protocol_version).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to parse msg: {e:?}"),
                    )
                })
            } else {
                connection.write().await.unsubscribe(handle.id).await;
                Err(Error::other(
                    "Channel Closed before response received",
                ))
            }
        }
    )
}
