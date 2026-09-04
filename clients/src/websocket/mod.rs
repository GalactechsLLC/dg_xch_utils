pub mod farmer;
pub mod full_node;
pub mod harvester;
pub mod introducer;
pub mod wallet;

use crate::ClientSSLConfig;
use async_trait::async_trait;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::outbound_limiter::{OutboundLimiter, ThrottleOutcome};
use dg_xch_core::protocols::rate_limits::RateLimiter;
use dg_xch_core::protocols::shared::{CAPABILITIES, Handshake, NoCertificateVerification};
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
use rustls::ClientConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use std::{env, fs};
use tokio::select;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, connect_async_tls_with_config};

fn large_message_ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(64 << 20))
        .max_frame_size(Some(64 << 20))
}
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
    /// Per-connection OUTBOUND self-throttle for messages WE send to this dialed peer.
    /// `Some` on full-node dials (`WsClientConfig::rate_limited`), `None`
    /// otherwise; see [`WsClient::send`].
    outbound_limiter: Option<Arc<OutboundLimiter>>,
    handle: JoinHandle<()>,
    run: Arc<AtomicBool>,
}
impl WsClient {
    pub async fn new(
        client_config: Arc<WsClientConfig>,
        node_type: NodeType,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        run: Arc<AtomicBool>,
        timeout: u64,
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
                timeout,
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
                timeout,
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
                timeout,
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
        timeout: u64,
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
            timeout,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn build(
        client_config: Arc<WsClientConfig>,
        node_type: NodeType,
        message_handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>,
        run: Arc<AtomicBool>,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        cert_str: &[u8],
        timeout_secs: u64,
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
        let (stream, _) = timeout(
            Duration::from_secs(timeout_secs),
            connect_async_tls_with_config(
                request,
                Some(large_message_ws_config()),
                false,
                Some(Connector::Rustls(Arc::new(
                    ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
                        .with_client_auth_cert(certs, key)
                        .map_err(|e| Error::other(format!("Error Building Client: {e:?}")))?,
                ))),
            ),
        )
        .await
        .map_err(|_| Error::other("Timeout Connecting Client"))?
        .map_err(|e| Error::other(format!("Error Connecting Client: {e:?}")))?;
        let peers = Arc::new(RwLock::new(HashMap::new()));
        let limiter = if client_config.rate_limited {
            Some(Arc::new(RateLimiter::new(true)))
        } else {
            None
        };
        let outbound_limiter = if client_config.rate_limited {
            Some(Arc::new(OutboundLimiter::new()))
        } else {
            None
        };
        let (ws_con, mut stream) = WebsocketConnection::new(
            WebsocketMsgStream::Tls(Box::new(stream)),
            message_handlers,
            peer_id.clone(),
            peers.clone(),
            limiter,
        );
        let v3 = ws_con.v3();
        let connection = Arc::new(RwLock::new(ws_con));
        let peer_capabilities = Arc::new(RwLock::new(Vec::new()));
        peers.write().await.insert(
            *peer_id.as_ref(),
            Arc::new(SocketPeer {
                node_type: Arc::new(RwLock::new(NodeType::Harvester)),
                protocol_version: Arc::new(RwLock::new(ChiaProtocolVersion::default())),
                capabilities: peer_capabilities.clone(),
                websocket: connection.clone(),
                host: client_config.host.parse::<std::net::IpAddr>().ok(),
                bans: None,
                outbound_limiter: outbound_limiter.clone(),
                v3,
            }),
        );
        let handle_run = run.clone();
        let protocol_version = client_config.protocol_version;
        let mut ws_client = WsClient {
            connection,
            client_config,
            handshake: None,
            outbound_limiter,
            handle: tokio::spawn(async move { stream.run(handle_run).await }),
            run,
        };
        ws_client
            .perform_handshake(node_type, protocol_version)
            .await?;
        // The server's handshake reply is consumed by the oneshot correlation path, not the handler,
        // so record its advertised capabilities on our own peer entry here — this is what the read
        // loop's rate limiter reads to pick the v1/v2 numbers for the peer we dialed.
        if let Some(hs) = ws_client.handshake.as_ref() {
            *peer_capabilities.write().await = hs.capabilities.clone();
        }
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

    /// Send `msg` to this dialed peer, self-throttling first when the outbound limiter is
    /// installed. The peer's negotiated capabilities (from its handshake reply) select the v1/v2
    /// numbers; a frequency-capped over-budget message is paced WITHOUT holding the connection
    /// write lock, so an `Unlimited` serve reply is never stalled behind it. A dropped message
    /// (exempt over budget, or the bounded attempt cap) is logged and swallowed. With no limiter
    /// this writes directly.
    pub async fn send(&self, msg: ChiaMessage) -> Result<(), Error> {
        if let Some(limiter) = &self.outbound_limiter {
            let caps = self
                .handshake
                .as_ref()
                .map(|hs| hs.capabilities.clone())
                .unwrap_or_default();
            let size = msg.data.as_slice().len();
            match limiter.admit(msg.msg_type, size, &caps).await {
                ThrottleOutcome::Admit => {}
                ThrottleOutcome::Drop(reason) => {
                    debug!(
                        "Self-rate-limiting outbound {:?} to dialed peer: {reason:?}",
                        msg.msg_type
                    );
                    return Ok(());
                }
            }
        }
        self.connection.write().await.send(msg.into()).await
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
            )?,
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
    /// Install the per-connection inbound rate limiter on this link.
    /// Set by the p2p full-node dialer; false for harvester/farmer/wallet client roles.
    pub rate_limited: bool,
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
        let _ = &self.channel.send(msg.data.bytes.clone()).await;
        Ok(())
    }
}

async fn acquire_v3_out_window(
    connection: &Arc<RwLock<WebsocketConnection>>,
    msg: &ChiaMessage,
    id: u16,
) -> Result<(), Error> {
    let v3 = connection.read().await.v3();
    if !v3.is_active() {
        return Ok(());
    }
    const WAIT_STEP_MS: u64 = 25;
    const WAIT_CAP_MS: u64 = 30_000;
    let mut waited: u64 = 0;
    loop {
        match v3.out_acquire(msg.msg_type, id) {
            Ok(_) => return Ok(()),
            Err(()) => {
                if waited >= WAIT_CAP_MS {
                    return Err(Error::new(
                        ErrorKind::TimedOut,
                        format!(
                            "peer's v3 in-flight window for {:?} stayed full for {WAIT_CAP_MS} ms",
                            msg.msg_type
                        ),
                    ));
                }
                tokio::time::sleep(Duration::from_millis(WAIT_STEP_MS)).await;
                waited += WAIT_STEP_MS;
            }
        }
    }
}

pub async fn oneshot_message(
    connection: Arc<RwLock<WebsocketConnection>>,
    mut msg: ChiaMessage,
    _custom_fn: Option<dg_xch_core::protocols::FilterFunction>,
    _msg_id: Option<u16>,
    timeout: Option<u64>,
) -> Result<Arc<ChiaMessage>, Error> {
    let mut request = connection.read().await.register_guarded_request();
    let id = request.id();
    msg.id = Some(id);
    acquire_v3_out_window(&connection, &msg, id).await?;
    if let Err(e) = connection.write().await.send(msg.into()).await {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("Failed to send data: {e:?}"),
        ));
    }
    match tokio::time::timeout(
        Duration::from_millis(timeout.unwrap_or(15000)),
        request.recv(),
    )
    .await
    {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(Error::other("Channel Closed before response received")),
        Err(_) => Err(Error::other("Timeout before oneshot_message completed")),
    }
}

pub async fn oneshot<R: ChiaSerialize>(
    connection: Arc<RwLock<WebsocketConnection>>,
    mut msg: ChiaMessage,
    resp_type: Option<ProtocolMessageTypes>,
    protocol_version: ChiaProtocolVersion,
    msg_id: Option<u16>,
    timeout: Option<u64>,
) -> Result<R, Error> {
    // Request/reply that carries a correlation id → connection-unique id + O(1) routing (the demux
    // fix). The id-less case (the Handshake verack, which correlates by type and happens once, before
    // any concurrent traffic exists) keeps the legacy type-filtered handler path below.
    if msg_id.is_some() || msg.id.is_some() {
        let mut request = connection.read().await.register_guarded_request();
        let id = request.id();
        msg.id = Some(id);
        acquire_v3_out_window(&connection, &msg, id).await?;
        if let Err(e) = connection.write().await.send(msg.into()).await {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Failed to parse send data: {e:?}"),
            ));
        }
        return match tokio::time::timeout(
            Duration::from_millis(timeout.unwrap_or(15000)),
            request.recv(),
        )
        .await
        {
            Ok(Ok(reply)) => {
                let mut cursor = Cursor::new(reply.data.bytes.as_slice());
                R::from_bytes(&mut cursor, protocol_version).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to parse msg: {e:?}"),
                    )
                })
            }
            Ok(Err(_)) => Err(Error::other("Channel Closed before response received")),
            Err(_) => Err(Error::other("Timeout before oneshot completed")),
        };
    }
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
            let resp = if let Some(v) = res {
                let retn = {
                    let values = v;
                    let mut cursor = Cursor::new(values.as_slice());
                    R::from_bytes(&mut cursor, protocol_version).map_err(|e| {
                        Error::new(
                            ErrorKind::InvalidData,
                            format!("Failed to parse msg: {e:?}"),
                        )
                    })?
                };
                Ok(retn)
            } else {
                Err(Error::other(
                    "Channel Closed before response received",
                ))
            };
            connection.read().await.unsubscribe(handle.id).await;
            resp
        }
    )
}
