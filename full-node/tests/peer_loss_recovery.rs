use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode, outbound_on_connect};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::PeerMap;
use dg_xch_core::protocols::full_node::NewPeak;
use dg_xch_p2p::{FullNodeApi, OutboundPeer, P2pSettings, dial, full_node_handlers};
use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// The peer's announced peak (H > 0). Header hash/weight are arbitrary — only that a live claim of
// height H is established, dropped, and then re-acquired after the reconnect greeting.
const PEAK_H: u32 = 5_000_000;
const PEAK_WEIGHT: u128 = 9_000_000_000;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_peerloss_{}_{n}.sqlite",
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
        trusted_peers: Vec::new(),
        trusted_cidrs: Vec::new(),
    }
}

fn free_addr() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr")
}

// A real full-node peer that greets EVERY dialing node with its peak, sent unconditionally on
// every new connection. This is what makes a drop + redial recoverable: the reconnected peer
// re-announces its tip with no solicitation, exactly as a real network peer does.
struct GreetingPeer {
    peak: NewPeak,
}

#[async_trait]
impl FullNodeApi for GreetingPeer {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    async fn full_node_peak(&self) -> Option<NewPeak> {
        Some(self.peak.clone())
    }
    // The node's own on-connect NewPeak (if any) lands here; irrelevant to this test.
    async fn on_new_peak(&self, _peer: Bytes32, _peak: NewPeak) {}
}

async fn spawn_greeting_peer() -> (u16, Arc<AtomicBool>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let port = free_addr().port();
    let api: Arc<dyn FullNodeApi> = Arc::new(GreetingPeer {
        peak: NewPeak {
            header_hash: Bytes32::from([7u8; 32]),
            height: PEAK_H,
            weight: PEAK_WEIGHT,
            fork_point_with_previous_peak: 0,
            unfinished_reward_block_hash: Bytes32::default(),
        },
    });
    let handlers = Arc::new(RwLock::new(full_node_handlers(
        api,
        "mainnet".to_string(),
        port,
    )));
    let peers: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    let server = WebsocketServer::new(
        &WebsocketServerConfig {
            host: "127.0.0.1".to_string(),
            port,
            ssl_info: None,
        },
        peers,
        handlers,
    )
    .expect("server");
    let run = Arc::new(AtomicBool::new(true));
    let run_c = run.clone();
    tokio::spawn(async move {
        let _ = server.run(run_c).await;
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    (port, run)
}

async fn observed_claimed(node: &Arc<FullNode>) -> u32 {
    node.state
        .live
        .get()
        .expect("live node state")
        .claimed_peak
        .load(Ordering::Relaxed)
}

// Poll `claimed_peak` for `want` up to `timeout` (bounded, no unbounded sleep racing the socket).
// Returns the last observed value — equal to `want` on success, otherwise whatever it settled at.
async fn wait_claimed(node: &Arc<FullNode>, want: u32, timeout: Duration) -> u32 {
    let start = Instant::now();
    let mut last = observed_claimed(node).await;
    while start.elapsed() < timeout {
        if last == want {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        last = observed_claimed(node).await;
    }
    last
}

// Dial the peer exactly as an outbound slot does — the production handler stack (per-connection
// ClaimGuard) over a real mTLS socket. Returns (client, handlers-map, run-flag) so the caller can
// tear the connection down deterministically (drop the guard) later.
async fn dial_as_outbound_slot(
    node: &Arc<FullNode>,
    peer_port: u16,
    settings: &P2pSettings,
) -> (
    dg_xch_clients::websocket::WsClient,
    Arc<RwLock<HashMap<uuid::Uuid, Arc<dg_xch_core::protocols::ChiaMessageHandler>>>>,
    Arc<AtomicBool>,
) {
    let handlers = Arc::new(RwLock::new((node.outbound_handler_factory())()));
    let run = Arc::new(AtomicBool::new(true));
    let client = dial(
        "127.0.0.1",
        peer_port,
        handlers.clone(),
        run.clone(),
        settings,
    )
    .await
    .expect("dial peer");
    (client, handlers, run)
}

// A lost peer must not permanently collapse the sync target: after the drop retracts the claim, a
// redial's greeting re-records it (`on_connect` symmetry, ).
#[tokio::test]
async fn claimed_peak_recovers_after_a_peer_drop_and_redial() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (peer_port, peer_run) = spawn_greeting_peer().await;

    // The production node: EMPTY store (no local peak), unsynced, RPC live state attached so the raw
    // claimed_peak gauge is observable.
    let node = Arc::new(
        FullNode::boot(config(free_addr(), free_addr()))
            .await
            .expect("boot"),
    );
    node.synced.store(false, Ordering::Relaxed);
    node.attach_rpc_live(Bytes32::default());

    let settings = P2pSettings::default();

    // ---- (a) baseline: a live peer claim sets claimed_peak to H ---------------------------------
    // Dialing the peer completes the handshake; the peer greets NewPeak(H); the node's outbound
    // NewPeak handler records the per-connection claim -> claimed_peak == H.
    let (client1, handlers1, run1) = dial_as_outbound_slot(&node, peer_port, &settings).await;
    let baseline = wait_claimed(&node, PEAK_H, Duration::from_secs(5)).await;
    assert_eq!(
        baseline, PEAK_H,
        "baseline: a live peer's volunteered peak sets claimed_peak to H"
    );

    // ---- (b) drop the peer connection: the claim retracts, claimed_peak collapses to 0 ----------
    // Stop the read loop and release every Arc holding the connection's ClaimGuard (the
    // disconnect retraction). This collapse is CORRECT — recovery must follow it.
    run1.store(false, Ordering::Relaxed);
    drop(client1);
    drop(handlers1);
    let collapsed = wait_claimed(&node, 0, Duration::from_secs(5)).await;
    assert_eq!(
        collapsed, 0,
        "drop retracts the only claim; claimed_peak rolls back to 0 (the ClaimGuard Drop)"
    );

    // ---- (c) reconnect: the redial's greeting re-records the claim ------------------------------
    // A fresh dial (the reclaim-on-drop redial, p2p/src/sessions/mod.rs) plus the production
    // on-connect hook `outbound_on_connect`. The greeting peer re-announces its peak on the new
    // connection (`on_connect`, no solicitation), and the node re-records the claim.
    let (client2, _handlers2, run2) = dial_as_outbound_slot(&node, peer_port, &settings).await;
    let peer2 = Arc::new(OutboundPeer {
        endpoint: ("127.0.0.1".to_string(), peer_port),
        client: client2,
        run: run2.clone(),
    });
    outbound_on_connect(&node, &peer2).await;

    let recovered = wait_claimed(&node, PEAK_H, Duration::from_secs(5)).await;
    assert_eq!(
        recovered, PEAK_H,
        "after a peer drop + redial the reconnected peer's greeting must re-acquire the sync \
         target (claimed_peak returns to H) — the on-connect symmetry"
    );

    peer_run.store(false, Ordering::Relaxed);
    run2.store(false, Ordering::Relaxed);
}
