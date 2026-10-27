use crate::protocol::{self, ChiaCertificateVerifier};
use dg_xch_core::{
    blockchain::{peer_info::TimestampedPeerInfo, unsized_bytes::UnsizedBytes},
    constants::CHIA_CA_KEY,
    protocols::{
        ChiaMessage, NodeType, ProtocolMessageTypes,
        full_node::{NewPeak, RequestPeers, RespondPeers},
        introducer::RequestPeersIntroducer,
        rate_limits::RateLimiter,
        shared::{ErrorMessage, Handshake},
    },
    ssl::{generate_ca_signed_cert_data, load_certs_from_bytes, load_private_key_from_bytes},
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Error},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
const LIMIT: usize = 1024 * 1024 + 8;

#[derive(Clone)]
pub struct Identity(pub Arc<rustls::ClientConfig>);
impl Identity {
    pub fn generate() -> Result<Self, Error> {
        let (cert, key) =
            generate_ca_signed_cert_data(protocol::CURRENT_CA.as_bytes(), CHIA_CA_KEY.as_bytes())?;
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        Ok(Self(Arc::new(
            rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(Error::other)?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(ChiaCertificateVerifier::new()?))
                .with_client_auth_cert(
                    load_certs_from_bytes(&cert)?,
                    load_private_key_from_bytes(&key)?,
                )
                .map_err(Error::other)?,
        )))
    }
}
pub use dg_xch_servers::discovery::{Observation, Phase as ProbePhase};

fn decode<T: ChiaSerialize>(bytes: &[u8], version: ChiaProtocolVersion) -> Result<T, Error> {
    if bytes.len() > LIMIT {
        return Err(Error::other("oversized protocol frame"));
    }
    T::from_bytes_exact(bytes, version)
}
fn envelope(bytes: &[u8], version: ChiaProtocolVersion) -> Result<ChiaMessage, Error> {
    if bytes.len() > LIMIT {
        return Err(Error::other("oversized protocol frame"));
    }
    // Unknown extension types do not affect the crawler's stable wire subset.
    let mut cursor = Cursor::new(bytes);
    let kind = u8::from_bytes(&mut cursor, version)?;
    let id = Option::<u16>::from_bytes(&mut cursor, version)?;
    let size = u32::from_bytes(&mut cursor, version)? as usize;
    let offset = cursor.position() as usize;
    if size != bytes.len().saturating_sub(offset) {
        return Err(Error::other("invalid protocol envelope length"));
    }
    Ok(ChiaMessage {
        msg_type: ProtocolMessageTypes::from(kind),
        id,
        data: UnsizedBytes::new(bytes[offset..].to_vec()),
    })
}
fn peer_list(
    bytes: &[u8],
    version: ChiaProtocolVersion,
) -> Result<Vec<TimestampedPeerInfo>, Error> {
    let count = u32::from_bytes(&mut Cursor::new(bytes), version)?;
    if count > 1000 {
        return Err(Error::other("peer response exceeds 1000 endpoints"));
    }
    Ok(decode::<RespondPeers>(bytes, version)?.peer_list)
}
async fn send<T: ChiaSerialize>(
    socket: &mut Socket,
    kind: ProtocolMessageTypes,
    body: &T,
    id: Option<u16>,
    version: ChiaProtocolVersion,
) -> Result<(), Error> {
    socket
        .send(Message::Binary(
            ChiaMessage::new(kind, version, body, id)?
                .to_bytes(version)?
                .into(),
        ))
        .await
        .map_err(Error::other)
}
async fn receive(
    socket: &mut Socket,
    kind: ProtocolMessageTypes,
    id: Option<u16>,
    obs: &mut Observation,
    version: ChiaProtocolVersion,
    limiter: &RateLimiter,
) -> Result<ChiaMessage, Error> {
    // Chia rate limits, frame size, and the enclosing deadline bound unsolicited traffic.
    for _ in 0..4096 {
        match socket.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                let message = envelope(&bytes, version)?;
                let no_capabilities = Vec::new();
                let caps = obs
                    .handshake
                    .as_ref()
                    .map_or(&no_capabilities, |h| &h.capabilities);
                if let Some(reason) =
                    limiter.process_and_check(message.msg_type, message.data.as_slice().len(), caps)
                {
                    return Err(Error::other(format!("peer rate limit exceeded: {reason}")));
                }
                if kind == ProtocolMessageTypes::Handshake
                    && (message.msg_type != kind || message.id.is_some())
                {
                    return Err(Error::other(
                        "expected an uncorrelated handshake as the first protocol message",
                    ));
                }
                if kind != ProtocolMessageTypes::Handshake
                    && message.msg_type == ProtocolMessageTypes::Handshake
                {
                    return Err(Error::other("duplicate protocol handshake"));
                }
                if id.is_some() && message.id == id {
                    if message.msg_type == ProtocolMessageTypes::Error {
                        let error: ErrorMessage = decode(message.data.as_slice(), version)?;
                        return Err(Error::other(format!(
                            "peer protocol error {}: {}",
                            error.code,
                            error.message.chars().take(128).collect::<String>()
                        )));
                    }
                    if message.msg_type == ProtocolMessageTypes::NoneResponse {
                        if !message.data.as_slice().is_empty() {
                            return Err(Error::other("invalid none_response body"));
                        }
                        return Err(Error::other("peer returned no response"));
                    }
                }
                if message.msg_type == kind && (id.is_none() || message.id == id) {
                    return Ok(message);
                }
                if message.msg_type == ProtocolMessageTypes::NewPeak {
                    let height = decode::<NewPeak>(message.data.as_slice(), version)?.height;
                    obs.peak_height = Some(height);
                } else if message.msg_type == ProtocolMessageTypes::RequestPeers {
                    decode::<RequestPeers>(message.data.as_slice(), version)?;
                    send(
                        socket,
                        ProtocolMessageTypes::RespondPeers,
                        &RespondPeers { peer_list: vec![] },
                        message.id,
                        version,
                    )
                    .await?;
                }
            }
            Some(Ok(Message::Ping(bytes))) => socket
                .send(Message::Pong(bytes))
                .await
                .map_err(Error::other)?,
            Some(Ok(Message::Close(_))) | None => {
                return Err(Error::other("peer closed connection"));
            }
            Some(Err(error)) => return Err(Error::other(error)),
            Some(Ok(Message::Text(_))) => {
                return Err(Error::other("expected binary Chia protocol message"));
            }
            _ => {}
        }
    }
    Err(Error::other("too many unsolicited frames"))
}
pub async fn probe(
    endpoint: SocketAddr,
    network: &str,
    identity: &Identity,
    deadline: Duration,
    policy: &dg_xch_servers::discovery::CrawlerPolicy,
) -> Observation {
    probe_controlled(endpoint, network, identity, deadline, policy, None).await
}
/// Production probes may transfer a slow peak wait into a shared verification pool.
pub async fn probe_controlled(
    endpoint: SocketAddr,
    network: &str,
    identity: &Identity,
    deadline: Duration,
    policy: &dg_xch_servers::discovery::CrawlerPolicy,
    control: Option<(
        &dg_xch_servers::discovery::Store,
        &dg_xch_servers::discovery::Claim,
    )>,
) -> Observation {
    let mut obs = Observation::default();
    if endpoint.is_ipv6() && !policy.enable_ipv6 {
        obs.error = Some("IPv6 probing is disabled".into());
        return obs;
    }
    if endpoint.port() != policy.port {
        obs.error = Some("endpoint port does not match the network peer port".into());
        return obs;
    }
    let connection = tokio::time::timeout(
        deadline,
        exchange(endpoint, network, identity, policy, &mut obs),
    )
    .await;
    obs.error = match connection {
        Ok(Ok(connection)) => finish_connection(connection, policy, &mut obs, control)
            .await
            .err()
            .map(|error| error.to_string()),
        Ok(Err(error)) => Some(error.to_string()),
        Err(_) => Some("probe timed out".into()),
    };
    obs
}
struct Connection {
    socket: Socket,
    version: ChiaProtocolVersion,
    limiter: RateLimiter,
    introducer: bool,
}

async fn exchange(
    endpoint: SocketAddr,
    network: &str,
    identity: &Identity,
    policy: &dg_xch_servers::discovery::CrawlerPolicy,
    obs: &mut Observation,
) -> Result<Connection, Error> {
    let started = Instant::now();
    let limiter = RateLimiter::new(true);
    let config = WebSocketConfig::default()
        .max_message_size(Some(LIMIT))
        .max_frame_size(Some(LIMIT));
    obs.phase = ProbePhase::TcpConnect;
    let tcp = tokio::time::timeout(
        Duration::from_secs(policy.tcp_connect_timeout_seconds),
        TcpStream::connect(endpoint),
    )
    .await
    .map_err(|_| Error::other("TCP connection timed out"))??;
    obs.phase = ProbePhase::Tls;
    let tls = TlsConnector::from(identity.0.clone())
        .connect(
            rustls::pki_types::ServerName::IpAddress(endpoint.ip().into()),
            tcp,
        )
        .await?;
    obs.tls_version = match tls.get_ref().1.protocol_version() {
        Some(rustls::ProtocolVersion::TLSv1_2) => Some("TLSv1.2".into()),
        Some(rustls::ProtocolVersion::TLSv1_3) => Some("TLSv1.3".into()),
        _ => None,
    };
    obs.node_id = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|certs| certs.first())
        .map(|cert| hex::encode(Sha256::digest(cert.as_ref())));
    obs.phase = ProbePhase::Websocket;
    let (mut socket, _) = client_async_with_config(
        format!("wss://{endpoint}/ws"),
        MaybeTlsStream::Rustls(tls),
        Some(config),
    )
    .await
    .map_err(Error::other)?;
    obs.phase = ProbePhase::Handshake;
    let hello = Handshake {
        network_id: network.into(),
        protocol_version: protocol::VERSION.to_string(),
        software_version: format!("dg_xch_crawler/{}", env!("CARGO_PKG_VERSION")),
        server_port: 0,
        node_type: NodeType::FullNode as u8,
        capabilities: protocol::CAPABILITIES
            .iter()
            .map(|(a, b)| (*a, (*b).into()))
            .collect(),
    };
    send(
        &mut socket,
        ProtocolMessageTypes::Handshake,
        &hello,
        None,
        protocol::VERSION,
    )
    .await?;
    let reply = receive(
        &mut socket,
        ProtocolMessageTypes::Handshake,
        None,
        obs,
        protocol::VERSION,
        &limiter,
    )
    .await?;
    let handshake: Handshake = decode(reply.data.as_slice(), protocol::VERSION)?;
    if handshake.network_id != network
        || ![NodeType::FullNode as u8, NodeType::Introducer as u8].contains(&handshake.node_type)
    {
        return Err(Error::other(
            "peer handshake has wrong network or unsupported node type",
        ));
    }
    let version = protocol::codec(&handshake)?;
    obs.connect_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    let introducer = handshake.node_type == NodeType::Introducer as u8;
    obs.handshake = Some(handshake);
    Ok(Connection {
        socket,
        version,
        limiter,
        introducer,
    })
}

async fn finish_connection(
    connection: Connection,
    policy: &dg_xch_servers::discovery::CrawlerPolicy,
    obs: &mut Observation,
    control: Option<(
        &dg_xch_servers::discovery::Store,
        &dg_xch_servers::discovery::Claim,
    )>,
) -> Result<(), Error> {
    let Connection {
        mut socket,
        version,
        limiter,
        introducer,
    } = connection;
    obs.phase = ProbePhase::PeerRequest;
    let started = Instant::now();
    let request = async {
        let kind = if introducer {
            send(
                &mut socket,
                ProtocolMessageTypes::RequestPeersIntroducer,
                &RequestPeersIntroducer {},
                Some(1),
                version,
            )
            .await?;
            ProtocolMessageTypes::RespondPeersIntroducer
        } else {
            send(
                &mut socket,
                ProtocolMessageTypes::RequestPeers,
                &RequestPeers {},
                Some(1),
                version,
            )
            .await?;
            ProtocolMessageTypes::RespondPeers
        };
        receive(&mut socket, kind, Some(1), obs, version, &limiter).await
    };
    // Include sending in the request deadline so a stalled writer cannot retain a worker.
    let response = tokio::time::timeout(Duration::from_secs(3), request).await;
    let request_error = match response {
        Ok(Ok(reply)) => match peer_list(reply.data.as_slice(), version) {
            Ok(peers) => {
                obs.peers = peers;
                obs.peer_request_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                None
            }
            Err(error) => Some(error),
        },
        Ok(Err(error)) => Some(error),
        Err(_) => Some(Error::other("peer request timed out")),
    };

    if !introducer
        && !obs
            .peak_height
            .is_some_and(|height| height >= policy.minimum_height)
    {
        obs.phase = ProbePhase::Verification;
        let result = {
            let wait = async {
                loop {
                    let reply = receive(
                        &mut socket,
                        ProtocolMessageTypes::NewPeak,
                        None,
                        obs,
                        version,
                        &limiter,
                    )
                    .await?;
                    let height = decode::<NewPeak>(reply.data.as_slice(), version)?.height;
                    obs.peak_height = Some(height);
                    if height >= policy.minimum_height {
                        return Ok::<_, Error>(());
                    }
                }
            };
            // This deadline starts after connection/discovery and is not truncated by it.
            tokio::pin!(wait);
            let quick_ms = if control.is_some() {
                policy.peak_wait_ms.min(2500)
            } else {
                policy.peak_wait_ms
            };
            let mut result = tokio::time::timeout(Duration::from_millis(quick_ms), &mut wait).await;
            if result.is_err()
                && quick_ms < policy.peak_wait_ms
                && let Some((store, claim)) = control
            {
                if store.reserve_verification(claim).await? {
                    result = tokio::time::timeout(
                        Duration::from_millis(policy.peak_wait_ms - quick_ms),
                        &mut wait,
                    )
                    .await;
                } else {
                    return Err(Error::other(
                        "peak verification deferred: slow-wait budget full",
                    ));
                }
            }
            result
        };
        if request_error.is_none() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) if obs.peak_height.is_none() => {
                    return Err(Error::other("NewPeak wait timed out"));
                }
                // Preserve the more useful below-minimum diagnostic below.
                Err(_) => {}
            }
        }
    }
    if let Some(error) = request_error {
        obs.phase = ProbePhase::PeerRequest;
        return Err(error);
    }
    if !introducer
        && obs
            .peak_height
            .is_some_and(|height| height < policy.minimum_height)
    {
        obs.phase = ProbePhase::Verification;
        return Err(Error::other(format!(
            "peer reported peak height {} below minimum {}",
            obs.peak_height.unwrap(),
            policy.minimum_height
        )));
    }
    let _ = tokio::time::timeout(Duration::from_millis(250), socket.close(None)).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_cni_streamable_fixtures_preserve_wire_values() {
        // Encoded independently with Python struct.pack using CNI's field definitions.
        let hello = hex::decode("000000076d61696e6e657400000006302e302e333300000006312e322e313120fc010000000100010000000131").unwrap();
        let peers = hex::decode("0000000200000007382e382e382e3820fc000000000000004d0000000f323630363a343730303a3a3131313120fc0000000000000058").unwrap();
        let peak = hex::decode("1111111111111111111111111111111111111111111111111111111111111111000493e000000000000100000000000000000007000493df2222222222222222222222222222222222222222222222222222222222222222").unwrap();
        for version in [
            ChiaProtocolVersion::Chia0_0_34,
            ChiaProtocolVersion::Chia0_0_35,
            ChiaProtocolVersion::Chia0_0_36,
        ] {
            let h: Handshake = decode(&hello, version).unwrap();
            assert_eq!(h.protocol_version, "0.0.33");
            assert_eq!(h.server_port, 8444);
            assert_eq!(
                protocol::codec(&h).unwrap(),
                ChiaProtocolVersion::Chia0_0_34
            );
            assert_eq!(h.to_bytes(version).unwrap(), hello);
            let list = peer_list(&peers, version).unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!(list[1].host, "2606:4700::1111");
            assert_eq!(list[1].timestamp, 88);
            let p: NewPeak = decode(&peak, version).unwrap();
            assert_eq!(p.height, 300000);
            assert_eq!(p.weight, (1u128 << 80) + 7);
            assert_eq!(p.to_bytes(version).unwrap(), peak);
        }
    }
    #[test]
    fn discovery_message_codes_and_request_envelopes_match_cni() {
        assert_eq!(ProtocolMessageTypes::Handshake as u8, 1);
        assert_eq!(ProtocolMessageTypes::NewPeak as u8, 20);
        assert_eq!(ProtocolMessageTypes::RespondPeers as u8, 44);
        assert_eq!(ProtocolMessageTypes::RespondPeersIntroducer as u8, 64);
        assert_eq!(ProtocolMessageTypes::Error as u8, 255);
        let full = ChiaMessage::new(
            ProtocolMessageTypes::RequestPeers,
            protocol::VERSION,
            &RequestPeers {},
            Some(1),
        )
        .unwrap()
        .to_bytes(protocol::VERSION)
        .unwrap();
        let intro = ChiaMessage::new(
            ProtocolMessageTypes::RequestPeersIntroducer,
            protocol::VERSION,
            &RequestPeersIntroducer {},
            Some(1),
        )
        .unwrap()
        .to_bytes(protocol::VERSION)
        .unwrap();
        assert_eq!(hex::encode(full), "2b01000100000000");
        assert_eq!(hex::encode(intro), "3f01000100000000");
    }
    #[test]
    fn malformed_frames_and_oversized_lists_are_rejected_before_decoding() {
        for bytes in [
            &[44, 2, 0, 0, 0, 0][..],
            &[44, 0, 0, 0, 0, 4, 0],
            &[44, 0, 0, 0, 0, 0, 1],
        ] {
            assert!(envelope(bytes, protocol::VERSION).is_err());
        }
        assert!(
            peer_list(&1001u32.to_be_bytes(), protocol::VERSION)
                .unwrap_err()
                .to_string()
                .contains("1000")
        );
        assert!(peer_list(&1u32.to_be_bytes(), protocol::VERSION).is_err());
    }
}
