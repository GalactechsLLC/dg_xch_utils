pub mod farmer;
pub mod full_node;
pub mod harvester;
pub mod wallet;

use crate::protocols::shared::{
    load_certs, load_certs_from_bytes, load_private_key, load_private_key_from_bytes, Handshake,
    NoCertificateVerification, CAPABILITIES, PROTOCOL_VERSION, SOFTWARE_VERSION,
};
use crate::protocols::ProtocolMessageTypes;
use async_trait::async_trait;
use dashmap::DashMap;
use dg_xch_core::ssl::{generate_ca_signed_cert_data, CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::ChiaSerialize;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt, TryFutureExt};
use hyper::header::{HeaderName, HeaderValue};
use hyper::upgrade::Upgraded;
use log::{debug, error, info, trace};
use rustls::ClientConfig;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Cursor, Error, ErrorKind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
#[cfg(target_os = "linux")]
use tokio::signal::unix::{signal, SignalKind};
#[cfg(target_os = "windows")]
use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::{fs, select};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};
use urlencoding::encode;
use uuid::Uuid;

#[cfg(target_os = "linux")]
pub async fn await_termination() -> Result<(), Error> {
    let mut term_signal = signal(SignalKind::terminate())?;
    let mut int_signal = signal(SignalKind::interrupt())?;
    let mut quit_signal = signal(SignalKind::quit())?;
    let mut alarm_signal = signal(SignalKind::alarm())?;
    let mut hup_signal = signal(SignalKind::hangup())?;
    select! {
        _ = term_signal.recv() => (),
        _ = int_signal.recv() => (),
        _ = quit_signal.recv() => (),
        _ = alarm_signal.recv() => (),
        _ = hup_signal.recv() => ()
    }
    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn await_termination() -> Result<(), Error> {
    let mut ctrl_break_signal = ctrl_break()?;
    let mut ctrl_c_signal = ctrl_c()?;
    let mut ctrl_close_signal = ctrl_close()?;
    let mut ctrl_logoff_signal = ctrl_logoff()?;
    let mut ctrl_shutdown_signal = ctrl_shutdown()?;
    select! {
        _ = ctrl_break_signal.recv() => (),
        _ = ctrl_c_signal.recv() => (),
        _ = ctrl_close_signal.recv() => (),
        _ = ctrl_logoff_signal.recv() => (),
        _ = ctrl_shutdown_signal.recv() => ()
    }
    Ok(())
}

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

pub async fn get_client_tls(
    host: &str,
    port: u16,
    ssl_info: ClientSSLConfig<'_>,
    additional_headers: &Option<HashMap<String, String>>,
) -> Result<(Client, ReadStream), Error> {
    let certs = load_certs(ssl_info.ssl_crt_path)?;
    let key = load_private_key(ssl_info.ssl_key_path)?;
    let cfg = Arc::new(
        ClientConfig::builder()
            .with_safe_defaults()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
            .with_client_auth_cert(certs, key)
            .map_err(|e| Error::new(ErrorKind::Other, format!("Error Building Client: {:?}", e)))?,
    );

    let connector = Connector::Rustls(cfg.clone());
    let mut request = format!("wss://{}:{}/ws", host, port)
        .into_client_request()
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Request: {}", e),
            )
        })?;
    if let Some(m) = additional_headers {
        for (k, v) in m {
            request.headers_mut().insert(
                HeaderName::from_str(k).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header Name {},\r\n {}", k, e),
                    )
                })?,
                HeaderValue::from_str(v).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header value {},\r\n {}", v, e),
                    )
                })?,
            );
        }
    }
    request.headers_mut().insert(
        HeaderName::from_str("chia-client-cert").map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Header Name chia-client-cert,\r\n {}", e),
            )
        })?,
        HeaderValue::from_str(
            encode(
                &fs::read_to_string(ssl_info.ssl_crt_path)
                    .await
                    .unwrap_or_default(),
            )
            .as_ref(),
        )
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Header value CHIA_CA_CRT,\r\n {}", e),
            )
        })?,
    );
    let (stream, resp) = connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Error Connecting Client: {:?}", e),
            )
        })?;
    debug!("Client Connect Resp: {:?}", resp);
    Ok(Client::new(stream))
}

pub async fn get_client_generated_tls(
    host: &str,
    port: u16,
    additional_headers: &Option<HashMap<String, String>>,
) -> Result<(Client, ReadStream), Error> {
    let (cert_bytes, key_bytes) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
            .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
    let certs = load_certs_from_bytes(&cert_bytes)?;
    let key = load_private_key_from_bytes(&key_bytes)?;
    let cfg = Arc::new(
        ClientConfig::builder()
            .with_safe_defaults()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
            .with_client_auth_cert(certs, key)
            .map_err(|e| Error::new(ErrorKind::Other, format!("Error Building Client: {:?}", e)))?,
    );
    let connector = Connector::Rustls(cfg.clone());
    let mut request = format!("wss://{}:{}/ws", host, port)
        .into_client_request()
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Request: {}", e),
            )
        })?;
    request.headers_mut().insert(
        HeaderName::from_str("chia-client-cert").map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Header Name chia-client-cert,\r\n {}", e),
            )
        })?,
        HeaderValue::from_str(encode(&String::from_utf8(cert_bytes).unwrap_or_default()).as_ref())
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to Parse Header value CHIA_CA_CRT,\r\n {}", e),
                )
            })?,
    );
    if let Some(m) = additional_headers {
        for (k, v) in m {
            request.headers_mut().insert(
                HeaderName::from_str(k).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header Name {},\r\n {}", k, e),
                    )
                })?,
                HeaderValue::from_str(v).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header value {},\r\n {}", v, e),
                    )
                })?,
            );
        }
    }
    let (stream, resp) = connect_async_tls_with_config(request, None, false, Some(connector))
        .await
        .map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Error Connecting Client: {:?}", e),
            )
        })?;
    debug!("Client Connect Resp: {:?}", resp);
    Ok(Client::new(stream))
}

pub async fn get_client(
    host: &str,
    port: u16,
    additional_headers: &Option<HashMap<String, String>>,
) -> Result<(Client, ReadStream), Error> {
    let mut request = format!("wss://{}:{}/ws", host, port)
        .into_client_request()
        .map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Request: {}", e),
            )
        })?;
    if let Some(m) = additional_headers {
        for (k, v) in m {
            request.headers_mut().insert(
                HeaderName::from_str(k).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header Name {},\r\n {}", k, e),
                    )
                })?,
                HeaderValue::from_str(v).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to Parse Header value {},\r\n {}", v, e),
                    )
                })?,
            );
        }
    }
    let (cert_bytes, _) =
        generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())
            .map_err(|e| Error::new(ErrorKind::Other, format!("OpenSSL Errors: {:?}", e)))?;
    request.headers_mut().insert(
        HeaderName::from_str("chia-client-cert").map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to Parse Header Name chia-client-cert,\r\n {}", e),
            )
        })?,
        HeaderValue::from_str(encode(&String::from_utf8(cert_bytes).unwrap_or_default()).as_ref())
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidData,
                    format!("Failed to Parse Header value CHIA_CA_CRT,\r\n {}", e),
                )
            })?,
    );
    let (stream, resp) = connect_async_tls_with_config(request, None, false, None)
        .await
        .map_err(|e| Error::new(ErrorKind::Other, e))?;
    debug!("Client Connect Resp: {:?}", resp);
    Ok(Client::new(stream))
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeType {
    Unknown = 0,
    FullNode = 1,
    Harvester = 2,
    Farmer = 3,
    Timelord = 4,
    Introducer = 5,
    Wallet = 6,
    DataLayer = 7,
}
impl From<u8> for NodeType {
    fn from(byte: u8) -> Self {
        match byte {
            i if i == NodeType::Unknown as u8 => NodeType::Unknown,
            i if i == NodeType::FullNode as u8 => NodeType::FullNode,
            i if i == NodeType::Harvester as u8 => NodeType::Harvester,
            i if i == NodeType::Farmer as u8 => NodeType::Farmer,
            i if i == NodeType::Timelord as u8 => NodeType::Timelord,
            i if i == NodeType::Introducer as u8 => NodeType::Introducer,
            i if i == NodeType::Wallet as u8 => NodeType::Wallet,
            i if i == NodeType::DataLayer as u8 => NodeType::DataLayer,
            _ => NodeType::Unknown,
        }
    }
}
#[derive(ChiaSerial, Debug, Clone)]
pub struct ChiaMessage {
    pub msg_type: ProtocolMessageTypes,
    pub id: Option<u16>,
    pub data: Vec<u8>,
}
impl ChiaMessage {
    pub fn new<T: ChiaSerialize>(msg_type: ProtocolMessageTypes, msg: &T, id: Option<u16>) -> Self {
        ChiaMessage {
            msg_type,
            id,
            data: msg.to_bytes(),
        }
    }
}
impl From<ChiaMessage> for Message {
    fn from(val: ChiaMessage) -> Self {
        Message::Binary(val.to_bytes())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HandshakeResp {
    pub handshake: Handshake,
    pub success: bool,
}

async fn perform_handshake(
    client: Arc<Mutex<Client>>,
    network_id: &str,
    port: u16,
    node_type: NodeType,
) -> Result<Handshake, Error> {
    oneshot::<Handshake, Client>(
        client,
        ChiaMessage::new(
            ProtocolMessageTypes::Handshake,
            &Handshake {
                network_id: network_id.to_string(),
                protocol_version: PROTOCOL_VERSION.to_string(),
                software_version: SOFTWARE_VERSION.to_string(),
                server_port: port,
                node_type: node_type as u8,
                capabilities: CAPABILITIES
                    .iter()
                    .map(|e| (e.0, e.1.to_string()))
                    .collect(),
            },
            None,
        ),
        Some(ProtocolMessageTypes::Handshake),
        None,
        Some(15000),
    )
    .await
}
#[derive(Debug)]
pub struct ChiaMessageFilter {
    pub msg_type: Option<ProtocolMessageTypes>,
    pub id: Option<u16>,
}
impl ChiaMessageFilter {
    pub fn matches(&self, msg: Arc<ChiaMessage>) -> bool {
        if self.id.is_some() && self.id != msg.id {
            return false;
        }
        if let Some(s) = &self.msg_type {
            if *s != msg.msg_type {
                return false;
            }
        }
        true
    }
}

pub struct ChiaMessageHandler {
    filter: ChiaMessageFilter,
    handle: Arc<dyn MessageHandler + Send + Sync>,
}
impl ChiaMessageHandler {
    pub fn new(filter: ChiaMessageFilter, handle: Arc<dyn MessageHandler + Send + Sync>) -> Self {
        ChiaMessageHandler { filter, handle }
    }
}

pub struct OneShotHandler {
    pub id: Uuid,
    channel: Sender<Vec<u8>>,
}
#[async_trait]
impl MessageHandler for OneShotHandler {
    async fn handle(&self, msg: Arc<ChiaMessage>) -> Result<(), Error> {
        debug!("{:?}", msg.as_ref());
        let _ = &self.channel.send(msg.data.clone()).await;
        Ok(())
    }
}

pub async fn oneshot<R: ChiaSerialize, C: Websocket>(
    client: Arc<Mutex<C>>,
    msg: ChiaMessage,
    resp_type: Option<ProtocolMessageTypes>,
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
    let chia_handle = ChiaMessageHandler {
        filter: ChiaMessageFilter {
            msg_type: resp_type,
            id: msg_id,
        },
        handle: handle.clone(),
    };
    client.lock().await.subscribe(handle.id, chia_handle).await;
    let res_handle = tokio::spawn(async move {
        let res = rx.recv().await;
        rx.close();
        res
    });
    client.lock().await.send(msg.into()).await.map_err(|e| {
        Error::new(
            ErrorKind::InvalidData,
            format!("Failed to parse send data: {:?}", e),
        )
    })?;
    select!(
        _ = tokio::time::sleep(Duration::from_millis(timeout.unwrap_or(15000))) => {
            client.lock().await.unsubscribe(handle.id).await;
            Err(Error::new(
                ErrorKind::Other,
                "Timeout before oneshot completed",
            ))
        }
        res = res_handle => {
            let res = res?;
            if let Some(v) = res {
                let mut cursor = Cursor::new(v);
                client.lock().await.unsubscribe(handle.id).await;
                R::from_bytes(&mut cursor).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidData,
                        format!("Failed to parse msg: {:?}", e),
                    )
                })
            } else {
                client.lock().await.unsubscribe(handle.id).await;
                Err(Error::new(
                    ErrorKind::Other,
                    "Channel Closed before response received",
                ))
            }
        }
    )
}

#[async_trait]
pub trait MessageHandler {
    async fn handle(&self, msg: Arc<ChiaMessage>) -> Result<(), Error>;
}

pub struct ReadStream {
    read: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    subscribers: Arc<DashMap<Uuid, ChiaMessageHandler>>,
}
impl ReadStream {
    pub async fn run(&mut self, run: Arc<AtomicBool>) {
        loop {
            select! {
                msg = self.read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(bin_data) => {
                                    let mut cursor = Cursor::new(bin_data);
                                    match ChiaMessage::from_bytes(&mut cursor) {
                                        Ok(chia_msg) => {
                                            let msg_arc: Arc<ChiaMessage> = Arc::new(chia_msg);
                                            for v in self.subscribers.as_ref() {
                                                if v.filter.matches(msg_arc.clone()) {
                                                    let msg_arc_c = msg_arc.clone();
                                                    let v_arc_c = v.handle.clone();
                                                    tokio::spawn(async move {
                                                        if let Err(e) = v_arc_c.handle(msg_arc_c.clone()).await {
                                                            error!("Error Handling Message: {:?}, {:?}", msg_arc_c, e);
                                                        }
                                                    });
                                                }
                                            }
                                            debug!("Processed Message: {:?}", &msg_arc.msg_type);
                                        }
                                        Err(e) => {
                                            error!("Invalid Message: {:?}", e);
                                        }
                                    }
                                },
                                Message::Close(reason) => {
                                    info!("Received Close: {:?}", reason);
                                    return;
                                }
                                _ => {
                                    error!("Invalid Message: {:?}", msg);
                                }
                            }
                        }
                        Some(Err(msg)) => {
                            match msg {
                                tokio_tungstenite::tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
                                    debug!("Client Stream Closed without Handshake");
                                },
                                others => {
                                    error!("Client Stream Error: {:?}", others);
                                }
                            }
                            return;
                        }
                        None => {
                            info!("End of client read Stream");
                            return;
                        }
                    }
                }
                _ = async {
                    loop {
                        if !run.load(Ordering::Relaxed) {
                            debug!("Client is exiting");
                            return;
                        } else {
                            tokio::time::sleep(Duration::from_secs(1)).await
                        }
                    }
                } => {
                    return;
                }
            }
        }
    }
}

#[async_trait]
pub trait Websocket {
    async fn send(&self, msg: Message) -> Result<(), Error>;
    async fn subscribe(&self, uuid: Uuid, handle: ChiaMessageHandler);
    async fn unsubscribe(&self, uuid: Uuid);
    async fn close(&self, msg: Option<Message>) -> Result<(), Error>;
}

pub struct ClientSSLConfig<'a> {
    pub ssl_crt_path: &'a str,
    pub ssl_key_path: &'a str,
    pub ssl_ca_crt_path: &'a str,
}
pub struct Client {
    write: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    subscribers: Arc<DashMap<Uuid, ChiaMessageHandler>>,
}
impl Client {
    pub fn new(stream: WebSocketStream<MaybeTlsStream<TcpStream>>) -> (Self, ReadStream) {
        let (write, read) = stream.split();
        let subscribers = Arc::new(DashMap::<Uuid, ChiaMessageHandler>::new());
        let client = Client {
            write: Arc::new(Mutex::new(write)),
            subscribers: subscribers.clone(),
        };
        let stream = ReadStream { read, subscribers };
        (client, stream)
    }
    pub async fn clear(&mut self) {
        self.subscribers.clear()
    }
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.subscribers.clear();
        self.close(None).await
    }
}
#[async_trait]
impl Websocket for Client {
    async fn send(&self, msg: Message) -> Result<(), Error> {
        trace!("Sending Request: {:?}", &msg);
        self.write
            .lock()
            .await
            .send(msg)
            .map_err(|e| Error::new(ErrorKind::Other, e))
            .await
    }

    async fn subscribe(&self, uuid: Uuid, handle: ChiaMessageHandler) {
        self.subscribers.insert(uuid, handle);
    }

    async fn unsubscribe(&self, uuid: Uuid) {
        self.subscribers.remove(&uuid);
    }

    async fn close(&self, msg: Option<Message>) -> Result<(), Error> {
        trace!("Sending Request: {:?}", &msg);
        if let Some(msg) = msg {
            let _ = self
                .write
                .lock()
                .await
                .send(msg)
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await;
            self.write
                .lock()
                .await
                .close()
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await
        } else {
            self.write
                .lock()
                .await
                .close()
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await
        }
    }
}

pub struct ServerReadStream {
    read: SplitStream<WebSocketStream<Upgraded>>,
    subscribers: Arc<DashMap<Uuid, ChiaMessageHandler>>,
}
impl ServerReadStream {
    pub async fn run(&mut self, run: Arc<AtomicBool>) {
        loop {
            select! {
                msg = self.read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Binary(bin_data) => {
                                    let mut cursor = Cursor::new(bin_data);
                                    match ChiaMessage::from_bytes(&mut cursor) {
                                        Ok(chia_msg) => {
                                            let msg_arc: Arc<ChiaMessage> = Arc::new(chia_msg);
                                            let mut matched = false;
                                            for v in self.subscribers.iter() {
                                                if v.filter.matches(msg_arc.clone()) {
                                                    let msg_arc_c = msg_arc.clone();
                                                    let v_arc_c = v.handle.clone();
                                                    tokio::spawn(async move { v_arc_c.handle(msg_arc_c).await });
                                                    matched = true;
                                                }
                                            }
                                            if !matched{
                                                error!("No Matches for Message: {:?}", &msg_arc);
                                            }
                                            debug!("Processed Message: {:?}", &msg_arc.msg_type);
                                        }
                                        Err(e) => {
                                            error!("Invalid Message: {:?}", e);
                                        }
                                    }
                                }
                                Message::Close(e) => {
                                    debug!("Server Got Close Message: {:?}", e);
                                    return;
                                },
                                _ => {
                                    error!("Invalid Message: {:?}", msg);
                                }
                            }
                        }
                        Some(Err(msg)) => {
                            match msg {
                                tokio_tungstenite::tungstenite::Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
                                    debug!("Server Stream Closed without Handshake");
                                },
                                others => {
                                    error!("Server Stream Error: {:?}", others);
                                }
                            }
                            return;
                        }
                        None => {
                            info!("End of server read Stream");
                            return;
                        }
                    }
                }
                _ = await_termination() => {
                    return;
                }
                _ = async {
                    loop {
                        if !run.load(Ordering::Relaxed){
                            debug!("Server is exiting");
                            return;
                        } else {
                            tokio::time::sleep(Duration::from_secs(1)).await
                        }
                    }
                } => {
                    return;
                }
            }
        }
    }
}

pub struct ServerConnection {
    write: Arc<Mutex<SplitSink<WebSocketStream<Upgraded>, Message>>>,
    subscribers: Arc<DashMap<Uuid, ChiaMessageHandler>>,
}
impl ServerConnection {
    pub fn new(stream: WebSocketStream<Upgraded>) -> (Self, ServerReadStream) {
        let (write, read) = stream.split();
        let subscribers = Arc::new(DashMap::<Uuid, ChiaMessageHandler>::new());
        let server = ServerConnection {
            write: Arc::new(Mutex::new(write)),
            subscribers: subscribers.clone(),
        };
        let stream = ServerReadStream { read, subscribers };
        (server, stream)
    }
    pub async fn clear(&mut self) {
        self.subscribers.clear()
    }
}
#[async_trait]
impl Websocket for ServerConnection {
    async fn send(&self, msg: Message) -> Result<(), Error> {
        trace!("Sending Request: {:?}", &msg);
        self.write
            .lock()
            .await
            .send(msg)
            .map_err(|e| Error::new(ErrorKind::Other, e))
            .await
    }

    async fn subscribe(&self, uuid: Uuid, handle: ChiaMessageHandler) {
        self.subscribers.insert(uuid, handle);
    }

    async fn unsubscribe(&self, uuid: Uuid) {
        self.subscribers.remove(&uuid);
    }

    async fn close(&self, msg: Option<Message>) -> Result<(), Error> {
        trace!("Sending Request: {:?}", &msg);
        if let Some(msg) = msg {
            let _ = self
                .write
                .lock()
                .await
                .send(msg)
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await;
            self.write
                .lock()
                .await
                .close()
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await
        } else {
            self.write
                .lock()
                .await
                .close()
                .map_err(|e| Error::new(ErrorKind::Other, e))
                .await
        }
    }
}
