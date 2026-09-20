#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::todo,
        clippy::unimplemented
    )
)]
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_core::protocols::introducer::{RequestPeersIntroducer, RespondPeersIntroducer};
use dg_xch_core::protocols::shared::Handshake;
use dg_xch_core::protocols::{ChiaMessage, NodeType, ProtocolMessageTypes};
use dg_xch_servers::transport::{
    SERVICE_MESSAGE_LIMIT, TlsIdentity, decode_exact, encode, handshake, shutdown_signal,
};
use futures_util::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub chain: ChainDefinition,
    pub tls: TlsIdentity,
    pub peer_server_name: String,
    #[serde(default)]
    pub allow_private_addresses: bool,
    pub max_connections: usize,
    pub max_peers: usize,
    pub peer_ttl_seconds: u64,
}

impl Config {
    pub fn validate(&self) -> Result<(), Error> {
        self.chain.constants().map_err(Error::other)?;
        ServerName::try_from(self.peer_server_name.clone()).map_err(Error::other)?;
        if self.listen.port() == 0
            || !(1..=256).contains(&self.max_connections)
            || !(1..=100_000).contains(&self.max_peers)
            || !(60..=86_400).contains(&self.peer_ttl_seconds)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid introducer resource limits",
            ));
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct PeerBook {
    peers: HashMap<SocketAddr, (Instant, u64)>,
    last_probe: HashMap<IpAddr, Instant>,
    rotation: usize,
}

impl PeerBook {
    fn expire(&mut self, now: Instant, ttl: Duration) {
        self.peers
            .retain(|_, entry| now.duration_since(entry.0) < ttl);
        self.last_probe
            .retain(|_, seen| now.duration_since(*seen) < Duration::from_secs(60));
    }

    fn begin_probe(&mut self, endpoint: SocketAddr, config: &Config, now: Instant) -> bool {
        self.expire(now, Duration::from_secs(config.peer_ttl_seconds));
        if !admissible_address(endpoint, config.allow_private_addresses)
            || self.last_probe.contains_key(&endpoint.ip())
            || self.last_probe.len() >= config.max_peers
            || (!self.peers.contains_key(&endpoint) && self.peers.len() >= config.max_peers)
        {
            return false;
        }
        self.last_probe.insert(endpoint.ip(), now);
        true
    }

    fn register(&mut self, endpoint: SocketAddr, config: &Config) {
        self.peers
            .retain(|known, _| known.ip() != endpoint.ip() || *known == endpoint);
        if self.peers.len() < config.max_peers || self.peers.contains_key(&endpoint) {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_secs());
            self.peers.insert(endpoint, (Instant::now(), timestamp));
        }
    }

    fn response(&mut self, requester: SocketAddr, config: &Config) -> Vec<TimestampedPeerInfo> {
        self.expire(Instant::now(), Duration::from_secs(config.peer_ttl_seconds));
        let mut candidates: Vec<_> = self
            .peers
            .iter()
            .filter(|(endpoint, _)| **endpoint != requester)
            .collect();
        candidates.sort_by_key(|entry| *entry.0);
        if !candidates.is_empty() {
            let offset = self.rotation % candidates.len();
            candidates.rotate_left(offset);
            self.rotation = self.rotation.wrapping_add(100);
        }
        candidates
            .into_iter()
            .take(100)
            .map(|(endpoint, (_, timestamp))| TimestampedPeerInfo {
                host: endpoint.ip().to_string(),
                port: endpoint.port(),
                timestamp: *timestamp,
            })
            .collect()
    }
}

pub fn admissible_address(endpoint: SocketAddr, allow_private: bool) -> bool {
    if endpoint.port() < 1024 || endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() {
        return false;
    }
    match endpoint.ip() {
        IpAddr::V4(address) => {
            if address.is_broadcast() || address.octets()[0] == 0 || address.octets()[0] >= 240 {
                return false;
            }
            let shared = address.octets()[0] == 100 && (64..=127).contains(&address.octets()[1]);
            let benchmark = address.octets()[0] == 198 && (18..=19).contains(&address.octets()[1]);
            let protocol_reserved =
                address.octets()[0] == 192 && address.octets()[1] == 0 && address.octets()[2] == 0;
            allow_private
                || !(address.is_private()
                    || address.is_loopback()
                    || address.is_link_local()
                    || address.is_documentation()
                    || shared
                    || benchmark
                    || protocol_reserved)
        }
        IpAddr::V6(address) => {
            if address.to_ipv4_mapped().is_some() {
                return false;
            }
            let reserved = (address.segments()[0] == 0x2001
                && (address.segments()[1] < 0x200 || address.segments()[1] == 0xdb8))
                || address.segments()[0] == 0x2002
                || address.segments()[0] == 0x3fff;
            allow_private
                || !(address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
                    || address.segments()[0] & 0xe000 != 0x2000
                    || reserved)
        }
    }
}

fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(SERVICE_MESSAGE_LIMIT))
        .max_frame_size(Some(SERVICE_MESSAGE_LIMIT))
}

async fn read_message<Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<Stream>,
) -> Result<ChiaMessage, Error> {
    match socket.next().await {
        Some(Ok(Message::Binary(bytes))) => decode_exact(&bytes),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "expected binary Chia protocol message",
        )),
    }
}

async fn probe(
    endpoint: SocketAddr,
    config: &Config,
    tls: Arc<rustls::ClientConfig>,
    network: &str,
) -> Result<(), Error> {
    let stream = TcpStream::connect(endpoint).await?;
    let name = ServerName::try_from(config.peer_server_name.clone()).map_err(Error::other)?;
    let stream = TlsConnector::from(tls).connect(name, stream).await?;
    let (mut socket, _) = tokio_tungstenite::client_async_with_config(
        format!("wss://{endpoint}/ws"),
        stream,
        Some(websocket_config()),
    )
    .await
    .map_err(Error::other)?;
    socket
        .send(encode(
            ProtocolMessageTypes::Handshake,
            &handshake(network, NodeType::FullNode, 0),
            None,
        )?)
        .await
        .map_err(Error::other)?;
    let message = read_message(&mut socket).await?;
    let greeting: Handshake = decode_exact(message.data.as_slice())?;
    if message.msg_type != ProtocolMessageTypes::Handshake
        || greeting.network_id != network
        || greeting.node_type != NodeType::FullNode as u8
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "endpoint is not a full node on this network",
        ));
    }
    Ok(())
}

async fn session(
    stream: TcpStream,
    remote: SocketAddr,
    config: Arc<Config>,
    acceptor: TlsAcceptor,
    client_tls: Arc<rustls::ClientConfig>,
    book: Arc<Mutex<PeerBook>>,
) -> Result<(), Error> {
    let stream = acceptor.accept(stream).await?;
    let mut socket = tokio_tungstenite::accept_async_with_config(stream, Some(websocket_config()))
        .await
        .map_err(Error::other)?;
    let message = read_message(&mut socket).await?;
    if message.msg_type != ProtocolMessageTypes::Handshake {
        return Err(Error::new(ErrorKind::InvalidData, "handshake required"));
    }
    let greeting: Handshake = decode_exact(message.data.as_slice())?;
    let network = config.chain.handshake_network_id().map_err(Error::other)?;
    if greeting.network_id != network || greeting.node_type != NodeType::FullNode as u8 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "wrong network or peer role",
        ));
    }
    socket
        .send(encode(
            ProtocolMessageTypes::Handshake,
            &handshake(&network, NodeType::Introducer, config.listen.port()),
            None,
        )?)
        .await
        .map_err(Error::other)?;
    let request = read_message(&mut socket).await?;
    if request.msg_type != ProtocolMessageTypes::RequestPeersIntroducer {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "introducer peer request required",
        ));
    }
    decode_exact::<RequestPeersIntroducer>(request.data.as_slice())?;
    let endpoint = SocketAddr::new(remote.ip(), greeting.server_port);
    let should_probe = book
        .lock()
        .await
        .begin_probe(endpoint, &config, Instant::now());
    if should_probe
        && matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                probe(endpoint, &config, client_tls, &network)
            )
            .await,
            Ok(Ok(()))
        )
    {
        book.lock().await.register(endpoint, &config);
    }
    let peer_list = book.lock().await.response(endpoint, &config);
    socket
        .send(encode(
            ProtocolMessageTypes::RespondPeersIntroducer,
            &RespondPeersIntroducer { peer_list },
            request.id,
        )?)
        .await
        .map_err(Error::other)?;
    socket.close(None).await.map_err(Error::other)
}

pub async fn serve(config: Config) -> Result<(), Error> {
    config.validate()?;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let acceptor = TlsAcceptor::from(config.tls.public_server()?);
    let client_tls = config.tls.client()?;
    let listener = TcpListener::bind(config.listen).await?;
    eprintln!(
        "Introducer listening on {} for {}",
        config.listen, config.chain.network_id
    );
    let config = Arc::new(config);
    let book = Arc::new(Mutex::new(PeerBook::default()));
    let mut sessions = JoinSet::new();
    let mut active = HashSet::new();
    loop {
        tokio::select! {
            result = shutdown_signal() => {
                result?;
                sessions.abort_all();
                while sessions.join_next().await.is_some() {}
                return Ok(());
            }
            Some(completed) = sessions.join_next(), if !sessions.is_empty() => {
                match completed {
                    Ok(address) => { active.remove(&address); }
                    Err(error) => return Err(Error::other(format!("introducer connection task failed: {error}"))),
                }
            }
            accepted = listener.accept() => {
                let (stream, remote) = accepted?;
                if sessions.len() >= config.max_connections || !active.insert(remote.ip()) {
                    continue;
                }
                let config = config.clone();
                let acceptor = acceptor.clone();
                let client_tls = client_tls.clone();
                let book = book.clone();
                sessions.spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(10), session(stream, remote, config, acceptor, client_tls, book)).await;
                    remote.ip()
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_policy_blocks_local_scan_targets_by_default() {
        for address in [
            "127.0.0.1:8444",
            "10.0.0.1:8444",
            "169.254.1.1:8444",
            "100.64.0.1:8444",
            "[::1]:8444",
            "[::ffff:8.8.8.8]:8444",
            "8.8.8.8:22",
            "224.0.0.1:8444",
        ] {
            assert!(
                !admissible_address(address.parse().unwrap(), false),
                "{address}"
            );
        }
        assert!(admissible_address("8.8.8.8:8444".parse().unwrap(), false));
        assert!(admissible_address("10.0.0.1:8444".parse().unwrap(), true));
        assert!(!admissible_address("0.0.0.0:8444".parse().unwrap(), true));
    }

    #[test]
    fn registry_caps_probes_expires_peers_and_excludes_requester() {
        let config = Config {
            listen: "127.0.0.1:8445".parse().unwrap(),
            chain: ChainDefinition::default(),
            tls: TlsIdentity {
                certificate: "unused".into(),
                private_key: "unused".into(),
                ca_certificate: "unused".into(),
            },
            peer_server_name: "localhost".to_owned(),
            allow_private_addresses: true,
            max_connections: 2,
            max_peers: 2,
            peer_ttl_seconds: 60,
        };
        let first = "10.0.0.1:8444".parse().unwrap();
        let second = "10.0.0.2:8444".parse().unwrap();
        let third = "10.0.0.3:8444".parse().unwrap();
        let mut book = PeerBook::default();
        let now = Instant::now();
        assert!(book.begin_probe(first, &config, now));
        assert!(!book.begin_probe(first, &config, now));
        book.register(first, &config);
        assert!(book.begin_probe(second, &config, now));
        book.register(second, &config);
        assert!(!book.begin_probe(third, &config, now));
        let response = book.response(first, &config);
        assert_eq!(response.len(), 1);
        assert_eq!(response[0].host, "10.0.0.2");
        book.expire(now + Duration::from_secs(61), Duration::from_secs(60));
        assert!(book.peers.is_empty());
        assert!(book.last_probe.is_empty());
    }
}
