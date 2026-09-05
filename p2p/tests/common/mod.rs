#![allow(dead_code)]
use async_trait::async_trait;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::websocket::WsClientConfig;
use dg_xch_clients::websocket::full_node::FullnodeClient;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::introducer::RespondPeersIntroducer;
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_p2p::FullNodeApi;
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
use std::collections::HashMap;
use std::io::Error;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use uuid::Uuid;

#[must_use]
pub fn load_full_block(height: u32) -> FullBlock {
    let raw = match height {
        5_000_000 => include_str!("../fixtures/full_block_5000000.json"),
        5_000_004 => include_str!("../fixtures/full_block_5000004.json"),
        other => panic!("no full-block fixture for height {other}"),
    };
    serde_json::from_str(raw).expect("full block fixture")
}

#[must_use]
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral")
        .local_addr()
        .expect("local addr")
        .port()
}

pub fn peer(host: &str, port: u16, ts: u64) -> TimestampedPeerInfo {
    TimestampedPeerInfo {
        host: host.to_string(),
        port,
        timestamp: ts,
    }
}

// Store-blind, in-memory backing for the handler set.
pub struct MemApi {
    pub blocks: HashMap<u32, FullBlock>,
    pub gossip: Vec<TimestampedPeerInfo>,
    pub respond_peers_seen: Arc<RwLock<Vec<TimestampedPeerInfo>>>,
}

#[async_trait]
impl FullNodeApi for MemApi {
    async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        self.blocks.get(&height).cloned().map(Box::new)
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        self.gossip.clone()
    }
    async fn on_respond_peers(&self, peers: Vec<TimestampedPeerInfo>) {
        self.respond_peers_seen.write().await.extend(peers);
    }
}

pub struct RunningServer {
    pub port: u16,
    pub run: Arc<AtomicBool>,
    pub handle: JoinHandle<()>,
    pub peers: PeerMap,
    pub bans: Arc<dg_xch_core::protocols::ban::BanRegistry>,
}

pub fn install_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// A MemApi holding `count` contiguous heights from `start`, each a clone of the 5_000_000 fixture.
#[must_use]
pub fn contiguous_api(start: u32, count: u32) -> Arc<MemApi> {
    let template = load_full_block(5_000_000);
    let mut blocks = HashMap::new();
    for h in start..start + count {
        blocks.insert(h, template.clone());
    }
    Arc::new(MemApi {
        blocks,
        gossip: Vec::new(),
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    })
}

fn client_config(port: u16, rate_limited: bool) -> Arc<WsClientConfig> {
    Arc::new(WsClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        server_port: 0,
        network_id: "mainnet".to_string(),
        ssl_info: None::<ClientSSLConfig>,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        rate_limited,
    })
}

// Dial a loopback full node and complete the handshake (no inbound limiter on this client).
pub async fn connect(port: u16) -> FullnodeClient {
    FullnodeClient::new(
        client_config(port, false),
        Arc::new(AtomicBool::new(true)),
        None,
        30,
    )
    .await
    .expect("client connects + handshakes")
}

// Dial without panicking: returns the handshake error instead of `.expect`-ing it, so a test can
// assert a banned host is REFUSED at the accept path (the connect fails).
pub async fn try_connect(port: u16) -> Result<FullnodeClient, Error> {
    FullnodeClient::new(
        client_config(port, false),
        Arc::new(AtomicBool::new(true)),
        None,
        5,
    )
    .await
}

// Same, but with the client-side inbound limiter active — the p2p dialer's posture, so the client's
// own read loop accounts the solicited replies it pulls (the item-5 self-safety path).
pub async fn rate_limited_client(port: u16) -> FullnodeClient {
    FullnodeClient::new(
        client_config(port, true),
        Arc::new(AtomicBool::new(true)),
        None,
        30,
    )
    .await
    .expect("rate-limited client connects + handshakes")
}

// Close every server-side peer connection and clear the map — simulates a peer drop
// (server restart / socket reset) as seen by the dialing client.
pub async fn drop_all_server_peers(server: &RunningServer) {
    let mut g = server.peers.write().await;
    for p in g.values() {
        let _ = p.websocket.write().await.close(None).await;
    }
    g.clear();
}

// Poll a condition to a deadline; returns true if it held before the timeout.
pub async fn wait_until<F, Fut>(mut cond: F, timeout: std::time::Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if cond().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    false
}

#[must_use]
pub fn fast_settings() -> dg_xch_p2p::P2pSettings {
    dg_xch_p2p::P2pSettings {
        target_outbound: 2,
        connect_timeout: std::time::Duration::from_secs(5),
        retry_timeout: std::time::Duration::from_millis(80),
        ..dg_xch_p2p::P2pSettings::default()
    }
}

pub fn empty_api() -> Arc<MemApi> {
    Arc::new(MemApi {
        blocks: HashMap::new(),
        gossip: vec![peer("1.1.1.1", 8444, 42), peer("2.2.2.2", 8444, 42)],
        respond_peers_seen: Arc::new(RwLock::new(Vec::new())),
    })
}

// Keepalive-tuned settings: short heartbeat + pong deadline so the teardown of a
// half-open peer is observable in a test (production defaults are 120s / 30s).
#[must_use]
pub fn keepalive_settings() -> dg_xch_p2p::P2pSettings {
    dg_xch_p2p::P2pSettings {
        heartbeat: std::time::Duration::from_millis(200),
        pong_deadline: std::time::Duration::from_millis(400),
        // wide reconnect backoff so the post-teardown zero window is observable
        retry_timeout: std::time::Duration::from_millis(700),
        ..fast_settings()
    }
}

// A node that completes the handshake but never answers RequestPeers — a half-open
// (silent) peer as far as the keepalive probe is concerned.
pub async fn spawn_silent_node() -> RunningServer {
    install_crypto();
    let port = free_port();
    let api: Arc<dyn FullNodeApi> = empty_api();
    let handler = Arc::new(dg_xch_p2p::FullNodeHandler {
        api,
        network_id: "mainnet".to_string(),
        server_port: port,
        respond_handshake: true,
        counters: Arc::default(),
    });
    let mut map: HashMap<Uuid, Arc<ChiaMessageHandler>> = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(dg_xch_core::protocols::ChiaMessageFilter {
                msg_type: Some(dg_xch_core::protocols::ProtocolMessageTypes::Handshake),
                id: None,
                custom_fn: None,
            }),
            handler,
        )),
    );
    spawn_with_handlers(port, map, false).await
}

pub async fn spawn_full_node(api: Arc<dyn FullNodeApi>) -> RunningServer {
    install_crypto();
    let port = free_port();
    let handlers = dg_xch_p2p::full_node_handlers(api, "mainnet".to_string(), port);
    spawn_with_handlers(port, handlers, false).await
}

// Same as `spawn_full_node` but with the per-connection inbound rate limiter active — the
// production full-node listener posture (the server sets `WebsocketServer::rate_limited = true`).
pub async fn spawn_full_node_rate_limited(api: Arc<dyn FullNodeApi>) -> RunningServer {
    install_crypto();
    let port = free_port();
    let handlers = dg_xch_p2p::full_node_handlers(api, "mainnet".to_string(), port);
    spawn_with_handlers(port, handlers, true).await
}

// A mock INTRODUCER (not a full node). The introducer speaks its own protocol — it answers
// `RequestPeersIntroducer` with `RespondPeersIntroducer`, NOT the full_node `RequestPeers`/`RespondPeers`.
// `seed_once` was corrected to speak this protocol, so the mock must too, or the seed gets no response and
// times out. Completes the handshake with the full_node handler (respond_handshake), then hands back
// `peer_list` on a RequestPeersIntroducer.
pub struct IntroducerMock {
    pub peer_list: Vec<TimestampedPeerInfo>,
    // Introducer-protocol queries served — the t046 retry tests assert cadence against it.
    pub queries: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait]
impl MessageHandler for IntroducerMock {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        peer_id: Arc<Bytes32>,
        peers: PeerMap,
    ) -> Result<(), Error> {
        self.queries
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let version = ChiaProtocolVersion::default();
        let resp = RespondPeersIntroducer {
            peer_list: self.peer_list.clone(),
        };
        if let Some(peer) = peers.read().await.get(peer_id.as_ref()).cloned() {
            let out = ChiaMessage::new(
                ProtocolMessageTypes::RespondPeersIntroducer,
                version,
                &resp,
                msg.id,
            )?;
            peer.websocket.write().await.send(out.into()).await?;
        }
        Ok(())
    }
}

// Spawn a mock introducer: the full_node handler completes the handshake, and a second handler answers
// `RequestPeersIntroducer` with the given `peer_list` — exercising the corrected one-shot seed path.
pub async fn spawn_introducer(peer_list: Vec<TimestampedPeerInfo>) -> RunningServer {
    let (server, _queries) = spawn_introducer_on(free_port(), peer_list).await;
    server
}

pub async fn spawn_introducer_on(
    port: u16,
    peer_list: Vec<TimestampedPeerInfo>,
) -> (RunningServer, Arc<std::sync::atomic::AtomicUsize>) {
    install_crypto();
    let api: Arc<dyn FullNodeApi> = empty_api();
    let handshake_handler = Arc::new(dg_xch_p2p::FullNodeHandler {
        api,
        network_id: "mainnet".to_string(),
        server_port: port,
        respond_handshake: true,
        counters: Arc::default(),
    });
    let mut map: HashMap<Uuid, Arc<ChiaMessageHandler>> = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: Some(ProtocolMessageTypes::Handshake),
                id: None,
                custom_fn: None,
            }),
            handshake_handler,
        )),
    );
    let queries = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: Some(ProtocolMessageTypes::RequestPeersIntroducer),
                id: None,
                custom_fn: None,
            }),
            Arc::new(IntroducerMock {
                peer_list,
                queries: queries.clone(),
            }),
        )),
    );
    (spawn_with_handlers(port, map, false).await, queries)
}

async fn spawn_with_handlers(
    port: u16,
    handlers_map: HashMap<Uuid, Arc<ChiaMessageHandler>>,
    rate_limited: bool,
) -> RunningServer {
    let handlers: Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>> =
        Arc::new(RwLock::new(handlers_map));
    let peers: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    let config = WebsocketServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        ssl_info: None,
    };
    let mut server = WebsocketServer::new(&config, peers.clone(), handlers).expect("build server");
    server.rate_limited = rate_limited;
    // Share the server's ban registry out so tests can observe/reset it (the accept path, the read
    // loop, and the close sites all key off this same instance).
    let bans = server.bans.clone();
    let run = Arc::new(AtomicBool::new(true));
    let run_c = run.clone();
    let handle = tokio::spawn(async move {
        let _ = server.run(run_c).await;
    });
    // give the listener a moment to bind
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    RunningServer {
        port,
        run,
        handle,
        peers,
        bans,
    }
}
