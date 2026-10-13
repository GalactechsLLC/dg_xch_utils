use dg_xch_core::{
    constants::{CHIA_CA_CRT, CHIA_CA_KEY},
    protocols::{
        ChiaMessage, NodeType, ProtocolMessageTypes,
        full_node::{NewPeak, RequestPeers, RespondPeers},
        introducer::{RequestPeersIntroducer, RespondPeersIntroducer},
        shared::{CAPABILITIES, Handshake, NoCertificateVerification},
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
const LIMIT: usize = 256 * 1024;

#[derive(Clone)]
pub struct Identity(pub Arc<rustls::ClientConfig>);
impl Identity {
    pub fn generate() -> Result<Self, Error> {
        let (cert, key) =
            generate_ca_signed_cert_data(CHIA_CA_CRT.as_bytes(), CHIA_CA_KEY.as_bytes())?;
        Ok(Self(Arc::new(
            // The shared CLI can enable both Rustls providers through other services.
            // Honor an installed provider, or explicitly choose the crate's default.
            rustls::ClientConfig::builder_with_provider(
                rustls::crypto::CryptoProvider::get_default()
                    .cloned()
                    .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider())),
            )
            .with_safe_default_protocol_versions()
            .map_err(Error::other)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification {}))
            .with_client_auth_cert(
                load_certs_from_bytes(&cert)?,
                load_private_key_from_bytes(&key)?,
            )
            .map_err(Error::other)?,
        )))
    }
}

pub use dg_xch_servers::discovery::{Observation, Phase as ProbePhase};

fn decode<T: ChiaSerialize>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > LIMIT {
        return Err(Error::other("oversized protocol frame"));
    }
    let mut cursor = Cursor::new(bytes);
    let value = T::from_bytes(&mut cursor, ChiaProtocolVersion::default())?;
    if cursor.position() != bytes.len() as u64 {
        return Err(Error::other("trailing protocol data"));
    }
    Ok(value)
}
async fn send<T: ChiaSerialize>(
    socket: &mut Socket,
    kind: ProtocolMessageTypes,
    body: &T,
    id: Option<u16>,
) -> Result<(), Error> {
    let version = ChiaProtocolVersion::default();
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
) -> Result<ChiaMessage, Error> {
    // Bound unsolicited traffic as well as frame size and overall wall time.
    for _ in 0..64 {
        match socket.next().await {
            Some(Ok(Message::Binary(bytes))) => {
                let message: ChiaMessage = decode(&bytes)?;
                if message.msg_type == kind && (id.is_none() || message.id == id) {
                    return Ok(message);
                }
                if message.msg_type == ProtocolMessageTypes::NewPeak {
                    obs.peak_height = Some(decode::<NewPeak>(message.data.as_slice())?.height);
                } else if message.msg_type == ProtocolMessageTypes::RequestPeers {
                    // We do not advertise unverified peers or a listening port.
                    send(
                        socket,
                        ProtocolMessageTypes::RespondPeers,
                        &RespondPeers { peer_list: vec![] },
                        message.id,
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
) -> Observation {
    let mut obs = Observation::default();
    let result =
        tokio::time::timeout(deadline, exchange(endpoint, network, identity, &mut obs)).await;
    obs.error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some("probe timed out".into()),
    };
    obs
}
async fn exchange(
    endpoint: SocketAddr,
    network: &str,
    identity: &Identity,
    obs: &mut Observation,
) -> Result<(), Error> {
    let started = Instant::now();
    let config = WebSocketConfig::default()
        .max_message_size(Some(LIMIT))
        .max_frame_size(Some(LIMIT));
    obs.phase = ProbePhase::TcpConnect;
    // Connect the SocketAddr directly so IPv6 never goes through hostname formatting.
    let tcp = TcpStream::connect(endpoint).await?;
    obs.phase = ProbePhase::Tls;
    let server_name = rustls::pki_types::ServerName::IpAddress(endpoint.ip().into());
    let tls = TlsConnector::from(identity.0.clone())
        .connect(server_name, tcp)
        .await?;
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
        protocol_version: ChiaProtocolVersion::default().to_string(),
        software_version: format!("dg_xch_crawler/{}", env!("CARGO_PKG_VERSION")),
        server_port: 0,
        node_type: NodeType::FullNode as u8,
        capabilities: CAPABILITIES
            .iter()
            .map(|(a, b)| (*a, (*b).into()))
            .collect(),
    };
    send(&mut socket, ProtocolMessageTypes::Handshake, &hello, None).await?;
    let reply = receive(&mut socket, ProtocolMessageTypes::Handshake, None, obs).await?;
    let handshake: Handshake = decode(reply.data.as_slice())?;
    if handshake.network_id != network
        || ![NodeType::FullNode as u8, NodeType::Introducer as u8].contains(&handshake.node_type)
    {
        return Err(Error::other(
            "peer handshake has wrong network or unsupported node type",
        ));
    }
    obs.connect_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    let introducer = handshake.node_type == NodeType::Introducer as u8;
    obs.handshake = Some(handshake);
    obs.phase = ProbePhase::PeerRequest;
    let started = Instant::now();
    let kind = if introducer {
        send(
            &mut socket,
            ProtocolMessageTypes::RequestPeersIntroducer,
            &RequestPeersIntroducer {},
            Some(1),
        )
        .await?;
        ProtocolMessageTypes::RespondPeersIntroducer
    } else {
        send(
            &mut socket,
            ProtocolMessageTypes::RequestPeers,
            &RequestPeers {},
            Some(1),
        )
        .await?;
        ProtocolMessageTypes::RespondPeers
    };
    let reply = receive(&mut socket, kind, Some(1), obs).await?;
    obs.peers = if introducer {
        decode::<RespondPeersIntroducer>(reply.data.as_slice())?.peer_list
    } else {
        decode::<RespondPeers>(reply.data.as_slice())?.peer_list
    };
    if obs.peers.len() > 1000 {
        return Err(Error::other("peer response exceeds 1000 endpoints"));
    }
    obs.peer_request_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
    // Dropping the socket promptly releases the network connection even if close stalls.
    let _ = tokio::time::timeout(Duration::from_millis(250), socket.close(None)).await;
    Ok(())
}
