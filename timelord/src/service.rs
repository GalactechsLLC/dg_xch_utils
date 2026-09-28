use crate::worker::{MAX_ITERATIONS, ProofRequest, ProofResult, run_isolated};
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_core::protocols::shared::Handshake;
use dg_xch_core::protocols::timelord::{
    NewPeakTimelord, RequestCompactProofOfTime, RespondCompactProofOfTime,
};
use dg_xch_core::protocols::{ChiaMessage, NodeType, ProtocolMessageTypes};
use dg_xch_servers::transport::{
    SERVICE_MESSAGE_LIMIT, TlsIdentity, decode_exact, encode, handshake, shutdown_signal,
};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub chain: ChainSelection,
    pub fullnode_host: String,
    pub fullnode_port: u16,
    pub server_name: String,
    pub tls: TlsIdentity,
    pub max_iterations: u64,
    pub job_timeout_seconds: u64,
    pub reconnect_seconds: u64,
    #[serde(default)]
    pub max_iterations_per_second: Option<u64>,
    #[serde(default = "default_worker_memory_bytes")]
    pub worker_memory_bytes: u64,
}

fn default_worker_memory_bytes() -> u64 {
    64 * 1024 * 1024
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        self.chain.constants().map_err(Error::other)?;
        ServerName::try_from(self.server_name.clone()).map_err(Error::other)?;
        if self.fullnode_host.is_empty()
            || self.fullnode_host.len() > 253
            || self.fullnode_port == 0
            || !(1..=MAX_ITERATIONS).contains(&self.max_iterations)
            || !(1..=86_400).contains(&self.job_timeout_seconds)
            || !(1..=300).contains(&self.reconnect_seconds)
            || self.max_iterations_per_second == Some(0)
            || !(128 * 1024..=8 * 1024 * 1024 * 1024).contains(&self.worker_memory_bytes)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid timelord configuration",
            ));
        }
        Ok(())
    }
}

pub(crate) type Socket =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

pub(crate) async fn connect(
    config: &Config,
    tls: Arc<rustls::ClientConfig>,
) -> Result<Socket, Error> {
    let network = config.chain.handshake_network_id().map_err(Error::other)?;
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(15), async {
        let stream =
            TcpStream::connect((config.fullnode_host.as_str(), config.fullnode_port)).await?;
        let name = ServerName::try_from(config.server_name.clone()).map_err(Error::other)?;
        let stream = TlsConnector::from(tls).connect(name, stream).await?;
        tokio_tungstenite::client_async_with_config(
            format!("wss://{}:{}/ws", config.server_name, config.fullnode_port),
            stream,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(SERVICE_MESSAGE_LIMIT))
                    .max_frame_size(Some(SERVICE_MESSAGE_LIMIT)),
            ),
        )
        .await
        .map_err(Error::other)
    })
    .await
    .map_err(|_| Error::new(ErrorKind::TimedOut, "timelord connection timed out"))??;
    socket
        .send(encode(
            ProtocolMessageTypes::Handshake,
            &handshake(&network, NodeType::Timelord, 0),
            None,
        )?)
        .await
        .map_err(Error::other)?;
    let response = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .map_err(|_| Error::new(ErrorKind::TimedOut, "timelord handshake timed out"))?;
    let Some(Ok(Message::Binary(bytes))) = response else {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "full node did not return a handshake",
        ));
    };
    let message: ChiaMessage = decode_exact(&bytes)?;
    let greeting: Handshake = decode_exact(message.data.as_slice())?;
    if message.msg_type != ProtocolMessageTypes::Handshake
        || greeting.network_id != network
        || greeting.node_type != NodeType::FullNode as u8
    {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "timelord connected to wrong network or role",
        ));
    }
    Ok(socket)
}

async fn session(config: &Config, tls: Arc<rustls::ClientConfig>) -> Result<(), Error> {
    let bits = usize::try_from(
        config
            .chain
            .constants()
            .map_err(Error::other)?
            .discriminant_size_bits,
    )
    .map_err(Error::other)?;
    let mut socket = connect(config, tls).await?;
    eprintln!("Authenticated full node; compact-proof mode");
    let mut generation = 0u64;
    let mut workers: JoinSet<Result<(RequestCompactProofOfTime, ProofResult), Error>> =
        JoinSet::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_message = tokio::time::Instant::now();
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_message.elapsed() > Duration::from_secs(90) {
                    return Err(Error::new(ErrorKind::TimedOut, "full node heartbeat expired"));
                }
                tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Ping(Vec::new().into()))).await.map_err(|_| Error::new(ErrorKind::TimedOut, "full node is not reading"))?.map_err(Error::other)?;
            }
            Some(completed) = workers.join_next(), if !workers.is_empty() => {
                match completed {
                    Ok(Ok((request, result))) if result.generation == generation => {
                        if result.info != request.new_proof_of_time {
                            eprintln!("Discarded compact proof with unexpected VDF output");
                            continue;
                        }
                        let response = RespondCompactProofOfTime { vdf_info: result.info, vdf_proof: result.proof, header_hash: request.header_hash, height: request.height, field_vdf: request.field_vdf };
                        tokio::time::timeout(Duration::from_secs(5), socket.send(encode(ProtocolMessageTypes::RespondCompactProofOfTime, &response, None)?)).await.map_err(|_| Error::new(ErrorKind::TimedOut, "compact response send timed out"))?.map_err(Error::other)?;
                    }
                    Ok(Err(error)) => eprintln!("Compact proof failed: {error}"),
                    Err(error) if !error.is_cancelled() => return Err(Error::other(error)),
                    _ => {}
                }
            }
            incoming = socket.next() => {
                last_message = tokio::time::Instant::now();
                let Some(incoming) = incoming else { return Err(Error::new(ErrorKind::ConnectionAborted, "full node disconnected")); };
                match incoming.map_err(Error::other)? {
                    Message::Binary(bytes) => {
                        let message: ChiaMessage = decode_exact(&bytes)?;
                        match message.msg_type {
                            ProtocolMessageTypes::NewPeakTimelord => {
                                let peak: NewPeakTimelord = decode_exact(message.data.as_slice())?;
                                if peak.difficulty == 0 || peak.sub_slot_iters == 0 {
                                    return Err(Error::new(ErrorKind::InvalidData, "invalid peak iteration parameters"));
                                }
                                generation = generation.checked_add(1).ok_or_else(|| Error::other("timelord generation overflow"))?;
                                workers.abort_all();
                            }
                            ProtocolMessageTypes::RequestCompactProofOfTime if workers.is_empty() => {
                                let request: RequestCompactProofOfTime = decode_exact(message.data.as_slice())?;
                                if !(1..=4).contains(&request.field_vdf) || request.new_proof_of_time.number_of_iterations > config.max_iterations {
                                    continue;
                                }
                                let work = ProofRequest { generation, challenge: request.new_proof_of_time.challenge, input: ClassgroupElement::get_default_element(), iterations: request.new_proof_of_time.number_of_iterations, discriminant_bits: bits };
                                work.validate()?;
                                let timeout = Duration::from_secs(config.job_timeout_seconds);
                                workers.spawn(async move { Ok((request, run_isolated(work, timeout).await?)) });
                            }
                            ProtocolMessageTypes::RequestCompactProofOfTime | ProtocolMessageTypes::NewUnfinishedBlockTimelord | ProtocolMessageTypes::NewGenesisTimelord => {}
                            _ => return Err(Error::new(ErrorKind::InvalidData, "unexpected timelord protocol message")),
                        }
                    }
                    Message::Ping(payload) => {
                        tokio::time::timeout(Duration::from_secs(5), socket.send(Message::Pong(payload))).await.map_err(|_| Error::new(ErrorKind::TimedOut, "pong send timed out"))?.map_err(Error::other)?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Err(Error::new(ErrorKind::ConnectionAborted, "full node closed connection")),
                    _ => return Err(Error::new(ErrorKind::InvalidData, "unexpected WebSocket message")),
                }
            }
        }
    }
}

pub async fn serve(config: Config) -> Result<(), Error> {
    config.validate()?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let tls = config.tls.client()?;
    loop {
        tokio::select! {
            result = shutdown_signal() => return result,
            result = session(&config, tls.clone()) => {
                if let Err(error) = result { eprintln!("Timelord disconnected: {error}"); }
            }
        }
        tokio::select! {
            result = shutdown_signal() => return result,
            _ = tokio::time::sleep(Duration::from_secs(config.reconnect_seconds)) => {}
        }
    }
}
