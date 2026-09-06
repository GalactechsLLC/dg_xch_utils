// The live announce-pull topology — the HALF tx_gossip.rs does NOT cover. There, the production
// StoreApi gates run on the SERVER side and a stub announcer dials in. Live, all eight peers are
// links WE dialed: announcements (signage points, EOS bundles, unfinished blocks) arrive on
// OUTBOUND links, so the production StoreApi hooks run on the CLIENT side and the pull request
// must go back out on the client's own socket. This test stands up exactly that: a mock peer
// serves; the node under test dials it with its production outbound handler stack
// (`FullNode::outbound_handler_factory`); the peer pushes `NewSignagePointOrEndOfSubSlot`; the test
// asserts the CLIENT socket emits `RequestSignagePointOrEndOfSubSlot` back (the announce →
// pull round trip).

mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::full_node::{
    NewSignagePointOrEndOfSubSlot, RequestSignagePointOrEndOfSubSlot,
};
use dg_xch_core::protocols::{ChiaMessage, PeerMap, ProtocolMessageTypes};
use dg_xch_p2p::{FullNodeApi, P2pSettings, SignagePointResponse, full_node_handlers};
use dg_xch_serialize::ChiaProtocolVersion;
use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
use dg_xch_stores::BlockStore;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_announce_pull_{}_{n}.sqlite",
        std::process::id()
    ));
    Config {
        rpc_tls: dg_full_node::RpcTlsMode::Local,
        debug_endpoints: false,
        p2p: Default::default(),
        listen,
        rpc,
        introducer: None,
        manual_peers: Vec::new(),
        advertise: None,
        backend: Backend::Sqlite(db),
        network_id: "mainnet".to_string(),
        capture_dir: None,
        genesis_sync: false,
        sync_from: 0,
        uncompact: false,
        prefetch_memory_mb: None,
        prefetch_max_inflight: None,
        performance: Default::default(),
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
    }
}

fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr")
}

// The mock peer's protocol surface: it captures any RequestSignagePointOrEndOfSubSlot the node
// under test pulls with (the assertion target) and otherwise serves nothing.
struct CapturingPeerApi {
    pulls: Arc<Mutex<Vec<RequestSignagePointOrEndOfSubSlot>>>,
}

#[async_trait]
impl FullNodeApi for CapturingPeerApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    async fn signage_point_or_eos(
        &self,
        req: RequestSignagePointOrEndOfSubSlot,
    ) -> Option<SignagePointResponse> {
        self.pulls.lock().expect("pulls lock").push(req);
        None
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_link_pulls_announced_signage_point() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // ---- the mock peer: a production-handler server that captures inbound pulls ----
    let peer_listen = free_addr();
    let pulls: Arc<Mutex<Vec<RequestSignagePointOrEndOfSubSlot>>> =
        Arc::new(Mutex::new(Vec::new()));
    let peer_api: Arc<dyn FullNodeApi> = Arc::new(CapturingPeerApi {
        pulls: pulls.clone(),
    });
    let peer_handlers = full_node_handlers(peer_api, "mainnet".to_string(), peer_listen.port());
    let peer_map: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    let server = WebsocketServer::new(
        &WebsocketServerConfig {
            host: peer_listen.ip().to_string(),
            port: peer_listen.port(),
            ssl_info: None,
        },
        peer_map.clone(),
        Arc::new(RwLock::new(peer_handlers)),
    )
    .expect("mock peer server");
    let server_run = Arc::new(AtomicBool::new(true));
    let server_run_c = server_run.clone();
    tokio::spawn(async move {
        let _ = server.run(server_run_c).await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // ---- the node under test dials OUT with its production outbound handler stack ----
    let node = Arc::new(
        FullNode::boot(config(free_addr(), free_addr()))
            .await
            .expect("boot"),
    );
    // Tip-synced posture: the SP/unfinished admission gates require it (the ignore-while-
    // syncing guard) — live, the follow driver sets this on every confirmed peak.
    node.synced.store(true, Ordering::Relaxed);
    let handlers = Arc::new(RwLock::new((node.outbound_handler_factory())()));
    let client_run = Arc::new(AtomicBool::new(true));
    let _client = dg_xch_p2p::dial(
        "127.0.0.1",
        peer_listen.port(),
        handlers,
        client_run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial mock peer");

    // ---- the peer announces a signage point on the link it accepted ----
    let mut inbound = None;
    for _ in 0..50 {
        if let Some(p) = peer_map.read().await.values().next().cloned() {
            inbound = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let inbound = inbound.expect("mock peer never registered the inbound link");
    let announce = NewSignagePointOrEndOfSubSlot {
        prev_challenge_hash: None,
        challenge_hash: Bytes32::const_new([1u8; 32]),
        index_from_challenge: 1,
        last_rc_infusion: Bytes32::const_new([2u8; 32]),
    };
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::NewSignagePointOrEndOfSubSlot,
        ChiaProtocolVersion::default(),
        &announce,
        None,
    )
    .expect("encode announce");
    inbound
        .websocket
        .write()
        .await
        .send(msg.into())
        .await
        .expect("push announce to client");

    // ---- the CLIENT socket must emit the pull (`new_signage_point_or_end_of_sub_slot`) ----
    let mut pulled = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(req) = pulls.lock().expect("pulls lock").first().cloned() {
            pulled = Some(req);
            break;
        }
    }
    let req = pulled.expect(
        "announced signage point was never pulled: the client-link dispatch dropped the reply",
    );
    assert_eq!(req.challenge_hash, announce.challenge_hash);
    assert_eq!(req.index_from_challenge, announce.index_from_challenge);
    assert_eq!(req.last_rc_infusion, announce.last_rc_infusion);

    client_run.store(false, Ordering::Relaxed);
    server_run.store(false, Ordering::Relaxed);
}

// The synced flag that GATES those pulls: current iff the last transaction block at-or-below
// the confirmed
// peak has a timestamp within the last 7 minutes. Confirming a HISTORICAL peak (the live node sat
// 8,338 blocks behind with the flag stuck true, pulling tip gossip it accepted 0 of) must NOT
// count as synced; a fresh-timestamped peak must.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn synced_flag_requires_a_current_chain_tip() {
    let node = Arc::new(
        FullNode::boot(config(free_addr(), free_addr()))
            .await
            .expect("boot"),
    );

    // Empty store: never synced.
    node.update_synced().await;
    assert!(
        !node.synced.load(Ordering::Relaxed),
        "empty store is not synced"
    );

    // The real mainnet fixture peak — a transaction block with a HISTORICAL timestamp. A follow
    // step confirming it is chain progress, but the chain is not current: not synced.
    let (rec, _block, _adds) = common::seed_peak(&node.store).await;
    assert!(rec.timestamp.is_some(), "fixture peak is a tx block");
    node.update_synced().await;
    assert!(
        !node.synced.load(Ordering::Relaxed),
        "a historical peak must not open the tip-gossip gates"
    );

    // Advance to a peak whose transaction timestamp is NOW: synced.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let mut fresh = rec.clone();
    fresh.height = rec.height + 1;
    fresh.prev_hash = rec.header_hash;
    fresh.header_hash = Bytes32::const_new([0xfe; 32]);
    fresh.timestamp = Some(now);
    node.store
        .add_block_records(std::slice::from_ref(&fresh))
        .await
        .expect("records");
    node.store.set_peak(&fresh.header_hash).await.expect("peak");
    node.update_synced().await;
    assert!(
        node.synced.load(Ordering::Relaxed),
        "a current-timestamped peak is synced"
    );
}
