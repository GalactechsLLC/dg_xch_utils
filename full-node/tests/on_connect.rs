// On-connect greetings, node side (`on_connect` :967-1010): the PRODUCTION node
// (FullNode::boot + build_peer_server, StoreApi over the real store/mempool) must greet a freshly
// handshaken FULL_NODE peer with
//   - NewPeak of the current peak (:991-998, fork_point_with_previous_peak = the peak height), and
//   - when synced, RequestMempoolTransactions carrying the BIP158 filter over OUR mempool ids
//     (:967-982, mempool_manager.get_filter :436-445);
// and must NOT send the mempool request while unsynced (`if synced and peak_height is not None`).
// The wallet greeting is covered by puzzle_state.rs; the dispatch-layer contract by p2p t047.
//
// Written RED: the StoreApi served neither greeting (the dispatch arm existed but the api's
// defaults answer None).

mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode};
use dg_xch_clients::websocket::{WsClient, WsClientConfig};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::full_node::{NewPeak, RequestMempoolTransactions};
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use std::collections::HashMap;
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use uuid::Uuid;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_onconnect_{}_{n}.sqlite",
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

struct PushCapture {
    tx: mpsc::Sender<Arc<ChiaMessage>>,
}

#[async_trait]
impl MessageHandler for PushCapture {
    async fn handle(
        &self,
        msg: Arc<ChiaMessage>,
        _peer_id: Arc<Bytes32>,
        _peers: PeerMap,
    ) -> Result<(), std::io::Error> {
        let _ = self.tx.send(msg).await;
        Ok(())
    }
}

type HandlerMap = Arc<RwLock<HashMap<Uuid, Arc<ChiaMessageHandler>>>>;

fn capture_handlers() -> (HandlerMap, mpsc::Receiver<Arc<ChiaMessage>>) {
    let (tx, rx) = mpsc::channel(16);
    let mut map = HashMap::new();
    map.insert(
        Uuid::new_v4(),
        Arc::new(ChiaMessageHandler::new(
            Arc::new(ChiaMessageFilter {
                msg_type: None,
                id: None,
                custom_fn: Some(Box::new(|m| {
                    matches!(
                        m.msg_type,
                        ProtocolMessageTypes::NewPeak
                            | ProtocolMessageTypes::RequestMempoolTransactions
                    )
                })),
            }),
            Arc::new(PushCapture { tx }),
        )),
    );
    (Arc::new(RwLock::new(map)), rx)
}

async fn dial_full_node(port: u16, handlers: HandlerMap) -> WsClient {
    let cfg = Arc::new(WsClientConfig {
        host: "127.0.0.1".to_string(),
        port,
        network_id: "mainnet".to_string(),
        ssl_info: None,
        software_version: None,
        protocol_version: ChiaProtocolVersion::default(),
        additional_headers: None,
        rate_limited: false,
    });
    WsClient::with_ca(
        cfg,
        NodeType::FullNode,
        handlers,
        Arc::new(AtomicBool::new(true)),
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        15,
    )
    .await
    .expect("full-node dial")
}

// The production node at the mainnet-fixture peak, peer server up, plus a dialed-in FULL_NODE
// peer with the greeting capture.
async fn rig(synced: bool) -> (Arc<FullNode>, WsClient, mpsc::Receiver<Arc<ChiaMessage>>) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    node.synced.store(synced, Ordering::Relaxed);
    let (server, run, _peers) = node.build_peer_server().expect("peer server");
    tokio::spawn(async move { server.run(run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (handlers, rx) = capture_handlers();
    let client = dial_full_node(listen.port(), handlers).await;
    (node, client, rx)
}

async fn recv_within(rx: &mut mpsc::Receiver<Arc<ChiaMessage>>, what: &str) -> Arc<ChiaMessage> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("{what} must arrive within 5s of the handshake"))
        .expect("capture channel open")
}

// The NewPeak greeting carries the store's confirmed peak, fork
// point = the peak height (the on-connect convention, same as the wallet greeting).
#[tokio::test]
async fn full_node_peer_is_greeted_with_the_confirmed_peak() {
    let (_node, _client, mut rx) = rig(true).await;

    let msg = recv_within(&mut rx, "the NewPeak greeting").await;
    assert_eq!(msg.msg_type, ProtocolMessageTypes::NewPeak);
    let got = NewPeak::from_bytes(
        &mut Cursor::new(msg.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("NewPeak decodes");
    let rec = common::peak_record();
    assert_eq!(got.header_hash, rec.header_hash, "peak hash");
    assert_eq!(got.height, common::PEAK_HEIGHT, "peak height");
    assert_eq!(got.weight, rec.weight, "peak weight");
    assert_eq!(
        got.fork_point_with_previous_peak,
        common::PEAK_HEIGHT,
        "on-connect fork point is the peak height"
    );
    // The unfinished reward-block hash commits to the peak's reward chain block
    // (`reward_chain_block.get_unfinished().get_hash()`).
    let unfinished = common::full_block().reward_chain_block.get_unfinished();
    let bytes = unfinished
        .to_bytes(ChiaProtocolVersion::default())
        .expect("serialize");
    assert_eq!(
        got.unfinished_reward_block_hash,
        Bytes32::from(dg_xch_core::utils::hash_256(&bytes)),
        "unfinished reward block hash"
    );
}

// Synced ⇒ RequestMempoolTransactions with our BIP158 filter (an
// empty mempool encodes the EMPTY PyBIP158 filter, which must still decode).
#[tokio::test]
async fn synced_node_requests_mempool_sync_from_a_new_full_node_peer() {
    let (_node, _client, mut rx) = rig(true).await;

    // One push is the NewPeak greeting, the other the mempool-sync request — order is not
    // part of the contract; accept either by collecting both.
    let a = recv_within(&mut rx, "the first on-connect push").await;
    let b = recv_within(&mut rx, "the second on-connect push").await;
    let req = [a, b]
        .into_iter()
        .find(|m| m.msg_type == ProtocolMessageTypes::RequestMempoolTransactions)
        .expect("one of the two on-connect pushes is RequestMempoolTransactions");
    let got = RequestMempoolTransactions::from_bytes(
        &mut Cursor::new(req.data.as_slice()),
        ChiaProtocolVersion::default(),
    )
    .expect("RequestMempoolTransactions decodes");
    assert!(
        dg_xch_core::consensus::block_filter::decode_chia_block_filter(&got.filter).is_some(),
        "the filter is a decodable BIP158 encoding (empty mempool → empty filter)"
    );
}

// ---- the OUTBOUND half: WE dial a full-node peer and must greet it too ----------------------
//
// on_connect fires for connections in BOTH directions (after the outgoing handshake too) —
// a node that only
// greets inbound peers never mempool-syncs from the peers IT dials, which on a fresh node is all
// of them. The server's supervisor on-connect hook runs `outbound_on_connect` against every
// outbound dial; this proves the sends against a recording mock peer.

// Spawn a mock full-node peer server that records the on-connect pushes it receives.
async fn spawn_recording_peer() -> (
    u16,
    Arc<AtomicBool>,
    Arc<RwLock<Vec<NewPeak>>>,
    Arc<RwLock<Vec<Vec<u8>>>>,
) {
    use dg_xch_servers::websocket::WebsocketServer;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let port = free_addr().port();
    let new_peaks: Arc<RwLock<Vec<NewPeak>>> = Arc::new(RwLock::new(Vec::new()));
    let mempool_filters: Arc<RwLock<Vec<Vec<u8>>>> = Arc::new(RwLock::new(Vec::new()));
    let api: Arc<dyn dg_xch_p2p::FullNodeApi> = Arc::new(PeerSideApi {
        new_peaks: new_peaks.clone(),
        mempool_filters: mempool_filters.clone(),
    });
    let handlers = dg_xch_p2p::full_node_handlers(api, "mainnet".to_string(), port);
    let handlers = Arc::new(RwLock::new(handlers));
    let peers: PeerMap = Arc::new(RwLock::new(HashMap::new()));
    let server = WebsocketServer::new(
        &dg_xch_servers::websocket::WebsocketServerConfig {
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
    (port, run, new_peaks, mempool_filters)
}

struct PeerSideApi {
    new_peaks: Arc<RwLock<Vec<NewPeak>>>,
    mempool_filters: Arc<RwLock<Vec<Vec<u8>>>>,
}

#[async_trait]
impl dg_xch_p2p::FullNodeApi for PeerSideApi {
    async fn block_by_height(
        &self,
        _height: u32,
    ) -> Option<Box<dg_xch_core::blockchain::full_block::FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<dg_xch_core::blockchain::peer_info::TimestampedPeerInfo> {
        Vec::new()
    }
    async fn on_new_peak(&self, _peer: Bytes32, peak: NewPeak) {
        self.new_peaks.write().await.push(peak);
    }
    async fn mempool_items(
        &self,
        filter: Vec<u8>,
    ) -> Vec<dg_xch_core::protocols::full_node::NewTransaction> {
        self.mempool_filters.write().await.push(filter);
        Vec::new()
    }
}

async fn wait_until<F, Fut>(mut cond: F, timeout: Duration) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if cond().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

// A synced node dialing OUT must greet the peer with NewPeak and request its mempool — the
// supervisor hook's send path, run against a real outbound link.
#[tokio::test]
async fn outbound_dial_greets_the_peer_and_requests_its_mempool() {
    let (peer_port, peer_run, new_peaks, mempool_filters) = spawn_recording_peer().await;

    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    node.synced.store(true, Ordering::Relaxed);

    // Dial exactly as an outbound slot dials (production handler stack), then run the hook body.
    let settings = dg_xch_p2p::P2pSettings::default();
    let handlers = Arc::new(RwLock::new((node.outbound_handler_factory())()));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        peer_port,
        handlers,
        Arc::new(AtomicBool::new(true)),
        &settings,
    )
    .await
    .expect("dial mock peer");
    let peer = Arc::new(dg_xch_p2p::OutboundPeer {
        endpoint: ("127.0.0.1".to_string(), peer_port),
        client,
        run: Arc::new(AtomicBool::new(true)),
    });

    dg_full_node::outbound_on_connect(&node, &peer).await;

    assert!(
        wait_until(
            || async { !new_peaks.read().await.is_empty() },
            Duration::from_secs(5)
        )
        .await,
        "the dialed peer must receive our NewPeak greeting"
    );
    let got = new_peaks.read().await[0].clone();
    assert_eq!(got.height, common::PEAK_HEIGHT);
    assert_eq!(got.header_hash, common::peak_record().header_hash);
    assert_eq!(got.fork_point_with_previous_peak, common::PEAK_HEIGHT);

    assert!(
        wait_until(
            || async { !mempool_filters.read().await.is_empty() },
            Duration::from_secs(5)
        )
        .await,
        "the dialed peer must receive our RequestMempoolTransactions (mempool sync on connect)"
    );
    let filter = mempool_filters.read().await[0].clone();
    assert!(
        dg_xch_core::consensus::block_filter::decode_chia_block_filter(&filter).is_some(),
        "the sent filter is a decodable BIP158 encoding"
    );

    peer_run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn unsynced_outbound_dial_sends_no_mempool_request() {
    let (peer_port, peer_run, new_peaks, mempool_filters) = spawn_recording_peer().await;

    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    node.synced.store(false, Ordering::Relaxed);

    let settings = dg_xch_p2p::P2pSettings::default();
    let handlers = Arc::new(RwLock::new((node.outbound_handler_factory())()));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        peer_port,
        handlers,
        Arc::new(AtomicBool::new(true)),
        &settings,
    )
    .await
    .expect("dial mock peer");
    let peer = Arc::new(dg_xch_p2p::OutboundPeer {
        endpoint: ("127.0.0.1".to_string(), peer_port),
        client,
        run: Arc::new(AtomicBool::new(true)),
    });

    dg_full_node::outbound_on_connect(&node, &peer).await;

    assert!(
        wait_until(
            || async { !new_peaks.read().await.is_empty() },
            Duration::from_secs(5)
        )
        .await,
        "the NewPeak greeting is unconditional"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        mempool_filters.read().await.is_empty(),
        "no mempool request while unsynced"
    );

    peer_run.store(false, Ordering::Relaxed);
}

#[tokio::test]
async fn unsynced_node_does_not_request_mempool_sync() {
    let (_node, _client, mut rx) = rig(false).await;

    let msg = recv_within(&mut rx, "the NewPeak greeting").await;
    assert_eq!(
        msg.msg_type,
        ProtocolMessageTypes::NewPeak,
        "the peak greeting is unconditional"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(600), rx.recv())
            .await
            .is_err(),
        "no RequestMempoolTransactions while unsynced"
    );
}
