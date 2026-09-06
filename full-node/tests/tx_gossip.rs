mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::protocols::full_node::{NewTransaction, RespondTransaction};
use dg_xch_core::protocols::{ChiaMessage, ProtocolMessageTypes};
use dg_xch_p2p::{FullNodeApi, P2pSettings, full_node_handlers_client};
use dg_xch_serialize::ChiaProtocolVersion;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;

static DBN: AtomicU64 = AtomicU64::new(0);

fn config(listen: SocketAddr, rpc: SocketAddr) -> Config {
    let n = DBN.fetch_add(1, Ordering::Relaxed);
    let db = std::env::temp_dir().join(format!(
        "full_node_gossip_{}_{n}.sqlite",
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

// The announcing peer's protocol surface: it holds one bundle and serves it back when the node
// under test pulls (the production client handler map turns RequestTransaction into
// RespondTransaction through this hook, exactly as a live outbound peer would).
struct AnnouncerApi {
    bundle: SpendBundle,
}

#[async_trait]
impl FullNodeApi for AnnouncerApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    async fn transaction(&self, id: Bytes32) -> Option<SpendBundle> {
        (self.bundle.name().ok()? == id).then(|| self.bundle.clone())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announced_transaction_is_pulled_validated_and_admitted() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // ---- the node under test: seeded to the real mainnet peak + one easy-puzzle coin ----
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    let coin = common::seed_easy_coin(&node.store, 1_000).await;
    node.mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    // Tip-synced posture: transaction gossip is gated on the synced flag (the
    // ignore-while-syncing guard) — this test exercises the pull-validate-admit path of an
    // AT-TIP node, same pattern as announce_pull.rs.
    node.synced.store(true, Ordering::Relaxed);
    let validator = node.clone();
    tokio::spawn(async move { validator.run_tx_validator().await });
    let (server, serve_run, _inbound_peers) = node.build_peer_server().expect("peer server");
    let listener_run = serve_run.clone();
    tokio::spawn(async move { server.run(listener_run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // ---- the announcing peer dials in on the production client handler stack ----
    let bundle = common::easy_bundle(&coin, 1);
    let name = bundle.name().expect("bundle name");
    let api: Arc<dyn FullNodeApi> = Arc::new(AnnouncerApi { bundle });
    let handlers = Arc::new(RwLock::new(full_node_handlers_client(
        api,
        "mainnet".to_string(),
        0,
    )));
    let run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        handlers,
        run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial node");

    // ---- one announcement; everything after this is the two handler stacks talking ----
    let announce = NewTransaction {
        transaction_id: name,
        cost: 1_000_000,
        fees: 1,
    };
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::NewTransaction,
        ChiaProtocolVersion::default(),
        &announce,
        None,
    )
    .expect("encode");
    client
        .connection
        .write()
        .await
        .send(msg.into())
        .await
        .expect("send announcement");

    // ---- the bundle must land in the node's mempool, validated server-side ----
    let mut admitted = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if node.mempool.lock().await.get(&name).is_some() {
            admitted = true;
            break;
        }
    }
    assert!(admitted, "announced bundle was not admitted within 5s");
    assert_eq!(node.mempool.lock().await.len(), 1);

    run.store(false, Ordering::Relaxed);
    serve_run.store(false, Ordering::Relaxed);
    node.run.store(false, Ordering::Relaxed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gossip_is_ignored_while_syncing() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    let coin = common::seed_easy_coin(&node.store, 1_000).await;
    node.mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    let (server, serve_run, _inbound_peers) = node.build_peer_server().expect("peer server");
    let listener_run = serve_run.clone();
    tokio::spawn(async move { server.run(listener_run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let bundle = common::easy_bundle(&coin, 1);
    let name = bundle.name().expect("bundle name");
    let api: Arc<dyn FullNodeApi> = Arc::new(AnnouncerApi { bundle });
    let handlers = Arc::new(RwLock::new(full_node_handlers_client(
        api,
        "mainnet".to_string(),
        0,
    )));
    let run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        handlers,
        run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial node");

    let announce = NewTransaction {
        transaction_id: name,
        cost: 1_000_000,
        fees: 1,
    };
    let msg = ChiaMessage::new(
        ProtocolMessageTypes::NewTransaction,
        ChiaProtocolVersion::default(),
        &announce,
        None,
    )
    .expect("encode");
    client
        .connection
        .write()
        .await
        .send(msg.into())
        .await
        .expect("send announcement");

    // Longer than the validator worker's 250 ms drain tick plus the positive test's admission
    // latency: nothing may be pulled or admitted.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        node.mempool.lock().await.get(&name).is_none(),
        "bundle admitted while not synced"
    );
    assert_eq!(node.mempool.lock().await.len(), 0);

    run.store(false, Ordering::Relaxed);
    serve_run.store(false, Ordering::Relaxed);
}

// Shared harness: seed an at-tip node, dial an announcing peer, return the pieces the ban/drop
// tests drive. `inbound_peers` is the node's live inbound map — a ban drops the peer from it, so
// `inbound_peers.read().await.len()` going 1 -> 0 is the observable that distinguishes a ban from
// an ignore (both leave the mempool empty).
struct Rig {
    node: Arc<FullNode>,
    coin: dg_xch_core::blockchain::coin::Coin,
    inbound_peers: dg_xch_core::protocols::PeerMap,
    serve_run: Arc<AtomicBool>,
    client: dg_xch_clients::websocket::WsClient,
    client_run: Arc<AtomicBool>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.client_run.store(false, Ordering::Relaxed);
        self.serve_run.store(false, Ordering::Relaxed);
        self.node.run.store(false, Ordering::Relaxed);
    }
}

async fn stand_up_rig() -> Rig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    let coin = common::seed_easy_coin(&node.store, 1_000).await;
    node.mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    node.synced.store(true, Ordering::Relaxed);
    let validator = node.clone();
    tokio::spawn(async move { validator.run_tx_validator().await });
    let (server, serve_run, inbound_peers) = node.build_peer_server().expect("peer server");
    let listener_run = serve_run.clone();
    tokio::spawn(async move { server.run(listener_run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A blind announcer: no served bundle, no request hook — these tests drive raw messages.
    let api: Arc<dyn FullNodeApi> = Arc::new(AnnouncerApi {
        bundle: common::easy_bundle(&coin, 1),
    });
    let handlers = Arc::new(RwLock::new(full_node_handlers_client(
        api,
        "mainnet".to_string(),
        0,
    )));
    let client_run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        handlers,
        client_run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial node");
    // Let the inbound side register the peer.
    for _ in 0..30 {
        if inbound_peers.read().await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Rig {
        node,
        coin,
        inbound_peers,
        serve_run,
        client,
        client_run,
    }
}

async fn send_msg<T: dg_xch_serialize::ChiaSerialize>(
    client: &dg_xch_clients::websocket::WsClient,
    msg_type: ProtocolMessageTypes,
    body: &T,
) {
    let msg =
        ChiaMessage::new(msg_type, ChiaProtocolVersion::default(), body, None).expect("encode");
    client
        .connection
        .write()
        .await
        .send(msg.into())
        .await
        .expect("send");
}

async fn peer_dropped_within(inbound_peers: &dg_xch_core::protocols::PeerMap, ms: u64) -> bool {
    let steps = ms / 50;
    for _ in 0..steps {
        if inbound_peers.read().await.is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    inbound_peers.read().await.is_empty()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_cost_announcement_bans_the_peer() {
    let rig = stand_up_rig().await;
    assert_eq!(rig.inbound_peers.read().await.len(), 1, "peer connected");
    let name = common::easy_bundle(&rig.coin, 1).name().expect("name");
    send_msg(
        &rig.client,
        ProtocolMessageTypes::NewTransaction,
        &NewTransaction {
            transaction_id: name,
            cost: 0,
            fees: 0,
        },
    )
    .await;
    assert!(
        peer_dropped_within(&rig.inbound_peers, 3000).await,
        "zero-cost announcer must be banned (dropped from the inbound peer map)"
    );
    rig.client_run.store(false, Ordering::Relaxed);
    rig.serve_run.store(false, Ordering::Relaxed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn already_seen_tx_with_mismatched_cost_bans_the_peer() {
    let rig = stand_up_rig().await;
    let bundle = common::easy_bundle(&rig.coin, 1);
    let name = bundle.name().expect("name");

    // Honest announce → the node pulls, validates, admits. The announcer serves the bundle back
    // through its AnnouncerApi::transaction hook when the RequestTransaction arrives.
    send_msg(
        &rig.client,
        ProtocolMessageTypes::NewTransaction,
        &NewTransaction {
            transaction_id: name,
            cost: 1_000_000,
            fees: 1,
        },
    )
    .await;
    let mut admitted_cost = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(item) = rig.node.mempool.lock().await.get(&name) {
            admitted_cost = Some(item.cost);
            break;
        }
    }
    let real_cost = admitted_cost.expect("bundle admitted so it is now a validated mempool item");
    assert_eq!(
        rig.inbound_peers.read().await.len(),
        1,
        "still connected after honest admit"
    );

    // Re-announce the SAME id with a cost that is neither the validated cost nor cost+tolerated.
    send_msg(
        &rig.client,
        ProtocolMessageTypes::NewTransaction,
        &NewTransaction {
            transaction_id: name,
            cost: real_cost + 1_000_000,
            fees: 1,
        },
    )
    .await;
    assert!(
        peer_dropped_within(&rig.inbound_peers, 3000).await,
        "mismatched re-announce of a seen tx must ban the peer"
    );
    rig.client_run.store(false, Ordering::Relaxed);
    rig.serve_run.store(false, Ordering::Relaxed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsolicited_transaction_body_is_dropped() {
    let rig = stand_up_rig().await;
    let bundle = common::easy_bundle(&rig.coin, 1);
    let name = bundle.name().expect("name");
    // No NewTransaction / RequestTransaction handshake precedes this — it is unsolicited.
    send_msg(
        &rig.client,
        ProtocolMessageTypes::RespondTransaction,
        &RespondTransaction {
            transaction: bundle,
        },
    )
    .await;
    // Longer than the validator's 250 ms drain: a solicited body would be admitted by now.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        rig.node.mempool.lock().await.get(&name).is_none(),
        "unsolicited transaction body must not be admitted"
    );
    assert_eq!(rig.node.mempool.lock().await.len(), 0);
    rig.client_run.store(false, Ordering::Relaxed);
    rig.serve_run.store(false, Ordering::Relaxed);
}

// ---- Admission/gossip gaps (mempool) ------------------------------------

// A capturing peer surface: records the ids the node PULLS (RequestTransaction reaching
// `transaction`) and the NewTransaction announcements the node pushes to us.
struct CapturingApi {
    bundle: SpendBundle,
    pulls: Arc<tokio::sync::Mutex<Vec<Bytes32>>>,
    new_txs: Arc<tokio::sync::Mutex<Vec<NewTransaction>>>,
}

#[async_trait]
impl FullNodeApi for CapturingApi {
    async fn block_by_height(&self, _height: u32) -> Option<Box<FullBlock>> {
        None
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
    async fn transaction(&self, id: Bytes32) -> Option<SpendBundle> {
        self.pulls.lock().await.push(id);
        (self.bundle.name().ok()? == id).then(|| self.bundle.clone())
    }
    async fn on_new_transaction(
        &self,
        _peer: Bytes32,
        tx: NewTransaction,
    ) -> dg_xch_p2p::TransactionAnnounceAction {
        self.new_txs.lock().await.push(tx);
        dg_xch_p2p::TransactionAnnounceAction::Ignore
    }
}

struct CapturingRig {
    node: Arc<FullNode>,
    coin: dg_xch_core::blockchain::coin::Coin,
    inbound_peers: dg_xch_core::protocols::PeerMap,
    client: dg_xch_clients::websocket::WsClient,
    pulls: Arc<tokio::sync::Mutex<Vec<Bytes32>>>,
    new_txs: Arc<tokio::sync::Mutex<Vec<NewTransaction>>>,
    _serve_run: Arc<AtomicBool>,
    _client_run: Arc<AtomicBool>,
}

impl Drop for CapturingRig {
    fn drop(&mut self) {
        self._client_run.store(false, Ordering::Relaxed);
        self._serve_run.store(false, Ordering::Relaxed);
        self.node.run.store(false, Ordering::Relaxed);
    }
}

async fn stand_up_capturing_rig() -> CapturingRig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    common::seed_peak(&node.store).await;
    let coin = common::seed_easy_coin(&node.store, 1_000).await;
    node.mempool.lock().await.set_peak(common::PEAK_HEIGHT, 0);
    node.synced.store(true, Ordering::Relaxed);
    let (server, serve_run, inbound_peers) = node.build_peer_server().expect("peer server");
    let listener_run = serve_run.clone();
    tokio::spawn(async move { server.run(listener_run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let pulls = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let new_txs = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let api: Arc<dyn FullNodeApi> = Arc::new(CapturingApi {
        bundle: common::easy_bundle(&coin, 1),
        pulls: pulls.clone(),
        new_txs: new_txs.clone(),
    });
    let handlers = Arc::new(RwLock::new(full_node_handlers_client(
        api,
        "mainnet".to_string(),
        0,
    )));
    let client_run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        handlers,
        client_run.clone(),
        &P2pSettings::default(),
    )
    .await
    .expect("dial node");
    for _ in 0..30 {
        if inbound_peers.read().await.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    CapturingRig {
        node,
        coin,
        inbound_peers,
        client,
        pulls,
        new_txs,
        _serve_run: serve_run,
        _client_run: client_run,
    }
}

// Fill the node's mempool to its 110B ceiling with synthetic high-fee items so is_fee_enough has
// to say no to weak fees (at_full_capacity + min-fee-rate).
async fn fill_mempool_to_capacity(node: &Arc<FullNode>) {
    use dg_xch_core::blockchain::coin::Coin;
    use dg_xch_core::blockchain::coin_record::CoinRecord;
    use dg_xch_core::blockchain::spend::Spend;
    use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
    use dg_xch_stores::CoinStore;
    for i in 0..20u8 {
        let coin = Coin {
            parent_coin_info: Bytes32::from([0xF0 ^ i; 32]),
            puzzle_hash: Bytes32::from([0x0F ^ i; 32]),
            amount: u64::MAX / 2,
        };
        let rec = CoinRecord {
            coin,
            confirmed_block_index: common::PEAK_HEIGHT,
            spent_block_index: 0,
            coinbase: false,
            timestamp: 0,
            spent: false,
        };
        node.store
            .apply_block(common::PEAK_HEIGHT, 0, std::slice::from_ref(&rec), &[])
            .await
            .expect("seed filler coin");
        let spend = Spend {
            parent_id: coin.parent_coin_info,
            coin_amount: coin.amount,
            puzzle_hash: coin.puzzle_hash,
            coin_id: coin.name(),
            height_relative: None,
            seconds_relative: None,
            before_height_relative: None,
            before_seconds_relative: None,
            birth_height: None,
            birth_seconds: None,
            create_coin: std::collections::HashSet::new(),
            agg_sig_me: vec![],
            agg_sig_parent: vec![],
            agg_sig_puzzle: vec![],
            agg_sig_amount: vec![],
            agg_sig_puzzle_amount: vec![],
            agg_sig_parent_amount: vec![],
            agg_sig_parent_puzzle: vec![],
            create_coin_announcements: vec![],
            assert_coin_announcements: vec![],
            create_puzzle_announcements: vec![],
            assert_puzzle_announcements: vec![],
            assert_concurrent_spend: vec![],
            assert_concurrent_puzzle: vec![],
            assert_ephemeral: false,
            sent_messages: vec![],
            received_messages: vec![],
            flags: 0,
            condition_cost: 0,
            execution_cost: 0,
        };
        let fee = 55_000_000_000u64; // fpc 10 at 5.5B cost
        let conds = SpendBundleConditions {
            spends: vec![spend],
            reserve_fee: fee,
            height_absolute: 0,
            seconds_absolute: 0,
            before_height_absolute: None,
            before_seconds_absolute: None,
            agg_sig_unsafe: vec![],
            cost: 5_500_000_000,
            removal_amount: u128::from(u64::MAX / 2),
            addition_amount: u128::from(u64::MAX / 2) - u128::from(fee),
        };
        let bundle = SpendBundle {
            coin_spends: vec![],
            aggregated_signature: dg_xch_core::blockchain::sized_bytes::Bytes96::from([i; 96]),
        };
        node.mempool
            .lock()
            .await
            .admit(node.store.as_ref(), bundle, conds)
            .await
            .expect("filler admitted");
    }
    let mp = node.mempool.lock().await;
    assert_eq!(mp.total_cost(), mp.max_total_cost(), "pool exactly full");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_pool_low_fee_announcement_is_not_pulled() {
    let rig = stand_up_capturing_rig().await;
    fill_mempool_to_capacity(&rig.node).await;

    // fpc 4 (< nonzero floor 5): must NOT trigger a RequestTransaction.
    send_msg(
        &rig.client,
        ProtocolMessageTypes::NewTransaction,
        &NewTransaction {
            transaction_id: Bytes32::from([0xAB; 32]),
            cost: 1_000_000,
            fees: 4_000_000,
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        rig.pulls.lock().await.is_empty(),
        "a fee that cannot get into the full pool must not be fetched"
    );

    // fpc 20 (> resident min fee rate 10): the pull fires.
    let bundle_id = common::easy_bundle(&rig.coin, 1).name().expect("name");
    send_msg(
        &rig.client,
        ProtocolMessageTypes::NewTransaction,
        &NewTransaction {
            transaction_id: bundle_id,
            cost: 1_000_000,
            fees: 20_000_000,
        },
    )
    .await;
    let mut pulled = false;
    for _ in 0..40 {
        if rig.pulls.lock().await.contains(&bundle_id) {
            pulled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        pulled,
        "a fee that beats the pool's min fee rate is fetched"
    );
}

// The announce sends NewTransaction to ALL full-node peers
// EXCLUDING the origin. Our announcer client is an INBOUND full-node peer of the node under
// test: it must receive announcements for transactions it did not originate, and never an echo
// of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn announce_drain_reaches_inbound_peers_and_excludes_origin() {
    let rig = stand_up_capturing_rig().await;

    struct NoOutbound;
    #[async_trait]
    impl dg_full_node::OutboundPeers for NoOutbound {
        async fn first_live(&self) -> Option<Arc<dg_xch_p2p::OutboundPeer>> {
            None
        }
        async fn live_peers(&self) -> Vec<Arc<dg_xch_p2p::OutboundPeer>> {
            Vec::new()
        }
    }
    let registry: Arc<dyn dg_full_node::OutboundPeers> = Arc::new(NoOutbound);

    // Not our transaction: the inbound peer must receive it.
    let x = Bytes32::from([0x77; 32]);
    rig.node.tx_announce.lock().await.push(NewTransaction {
        transaction_id: x,
        cost: 1_000_000,
        fees: 5_000_000,
    });
    rig.node.drain_tx_announcements(&registry).await;
    let mut got_x = false;
    for _ in 0..40 {
        if rig
            .new_txs
            .lock()
            .await
            .iter()
            .any(|t| t.transaction_id == x)
        {
            got_x = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        got_x,
        "NewTransaction re-broadcast must reach INBOUND full-node peers"
    );

    // Our own transaction: the origin peer must NOT get an echo.
    let peer_id = *rig
        .inbound_peers
        .read()
        .await
        .keys()
        .next()
        .expect("one inbound peer");
    let y = Bytes32::from([0x78; 32]);
    // Inbound origin: excluded by its exact cert-hash dispatch id (host `None` — the inbound path
    // keys on the id, not the host).
    rig.node.note_tx_origin(y, peer_id, None).await;
    rig.node.tx_announce.lock().await.push(NewTransaction {
        transaction_id: y,
        cost: 1_000_000,
        fees: 5_000_000,
    });
    rig.node.drain_tx_announcements(&registry).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !rig.new_txs
            .lock()
            .await
            .iter()
            .any(|t| t.transaction_id == y),
        "the origin peer must not receive an echo of its own transaction"
    );
}

// `request_mempool_transactions` + mempool_manager
// .get_items_not_in_filter: the peer's BIP158 filter is DECODED and honored — items
// the peer already holds are not re-announced; items outside the filter are.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_mempool_transactions_honors_bip158_filter() {
    use dg_xch_core::consensus::block_filter::chia_block_filter;
    let rig = stand_up_capturing_rig().await;

    // A real resident item: the easy bundle, validated exactly as the admission path does.
    let bundle = common::easy_bundle(&rig.coin, 1);
    let conds = dg_xch_core::consensus::block_generator::conditions_from_spend_bundle(
        &bundle,
        common::PEAK_HEIGHT + 1,
        &dg_xch_core::consensus::constants::MAINNET,
    )
    .expect("bundle validates");
    let resident = rig
        .node
        .mempool
        .lock()
        .await
        .admit(rig.node.store.as_ref(), bundle, conds)
        .await
        .expect("admitted");

    // Filter CONTAINING the resident id: nothing may be announced back.
    use dg_xch_core::traits::SizedBytes;
    let holds_it = chia_block_filter(&[resident.bytes().to_vec()]);
    send_msg(
        &rig.client,
        ProtocolMessageTypes::RequestMempoolTransactions,
        &dg_xch_core::protocols::full_node::RequestMempoolTransactions { filter: holds_it },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !rig.new_txs
            .lock()
            .await
            .iter()
            .any(|t| t.transaction_id == resident),
        "an item IN the peer's filter must not be re-announced"
    );

    // Filter of something else entirely: the resident item IS announced.
    let other = chia_block_filter(&[vec![0xEE; 32]]);
    send_msg(
        &rig.client,
        ProtocolMessageTypes::RequestMempoolTransactions,
        &dg_xch_core::protocols::full_node::RequestMempoolTransactions { filter: other },
    )
    .await;
    let mut announced = false;
    for _ in 0..40 {
        if rig
            .new_txs
            .lock()
            .await
            .iter()
            .any(|t| t.transaction_id == resident)
        {
            announced = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(announced, "an item OUTSIDE the peer's filter is announced");
}
