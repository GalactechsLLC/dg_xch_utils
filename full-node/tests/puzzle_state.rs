#![cfg(feature = "hint")]

mod common;

use async_trait::async_trait;
use dg_full_node::{Backend, Config, FullNode, WalletUpdate};
use dg_xch_clients::websocket::{WsClient, WsClientConfig, oneshot_message};
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::constants::{CHIA_CA_CRT, CHIA_CA_KEY};
use dg_xch_core::protocols::wallet::{
    CoinState, CoinStateFilters, CoinStateUpdate, NewPeakWallet, RejectCoinState,
    RejectPuzzleState, RejectStateReason, RequestCoinState, RequestPuzzleState,
    RequestRemoveCoinSubscriptions, RequestRemovePuzzleSubscriptions, RespondCoinState,
    RespondPuzzleState, RespondRemoveCoinSubscriptions, RespondRemovePuzzleSubscriptions,
};
use dg_xch_core::protocols::{
    ChiaMessage, ChiaMessageFilter, ChiaMessageHandler, MessageHandler, NodeType, PeerMap,
    ProtocolMessageTypes, WebsocketConnection,
};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::CoinStore;
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
        "full_node_puzstate_{}_{n}.sqlite",
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

// Captures the node→wallet pushes (NewPeakWallet, CoinStateUpdate) the way Sage's message channel
// receives them — registered on the wallet client's handler map BEFORE the handshake, so the
// on-connect push cannot race past it.
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

fn push_capture_handlers() -> (HandlerMap, mpsc::Receiver<Arc<ChiaMessage>>) {
    let (tx, rx) = mpsc::channel(64);
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
                        ProtocolMessageTypes::NewPeakWallet | ProtocolMessageTypes::CoinStateUpdate
                    )
                })),
            }),
            Arc::new(PushCapture { tx }),
        )),
    );
    (Arc::new(RwLock::new(map)), rx)
}

async fn dial_wallet(port: u16, handlers: HandlerMap) -> WsClient {
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
        NodeType::Wallet,
        handlers,
        Arc::new(AtomicBool::new(true)),
        CHIA_CA_CRT.as_bytes(),
        CHIA_CA_KEY.as_bytes(),
        15,
    )
    .await
    .expect("wallet dial")
}

// A node at the mainnet-fixture peak serving the production handler stack, plus a dialed-in
// wallet-type client with a push-capture channel.
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
    let (handlers, rx) = push_capture_handlers();
    let client = dial_wallet(listen.port(), handlers).await;
    (node, client, rx)
}

// Send `body` as `req_type` and await the correlated reply (EVERY one of these four requests
// is answered — a silent drop is the failure mode this suite guards).
async fn request<T: ChiaSerialize>(
    connection: &Arc<RwLock<WebsocketConnection>>,
    req_type: ProtocolMessageTypes,
    body: &T,
) -> Result<Arc<ChiaMessage>, std::io::Error> {
    let msg = ChiaMessage::new(req_type, ChiaProtocolVersion::default(), body, Some(1))?;
    oneshot_message(connection.clone(), msg, None, None, Some(10_000)).await
}

fn decode_reply<R: ChiaSerialize>(reply: &ChiaMessage, expect: ProtocolMessageTypes) -> R {
    assert_eq!(
        reply.msg_type, expect,
        "expected {expect:?}, got {:?}",
        reply.msg_type
    );
    let mut cursor = Cursor::new(reply.data.bytes.as_slice());
    R::from_bytes(&mut cursor, ChiaProtocolVersion::default()).expect("reply decodes")
}

fn all_filters() -> CoinStateFilters {
    // Sage's sync loop: CoinStateFilters::new(true, true, true, 0) (wallet_sync.rs:183).
    CoinStateFilters {
        include_spent: true,
        include_unspent: true,
        include_hinted: true,
        min_amount: 0,
    }
}

fn peak_header_hash() -> Bytes32 {
    common::peak_record().header_hash
}

// ── 1a. The headline gap (RED: no dispatch arm — RequestPuzzleState timed out) ───────────────────
// Sage's first-sync request shape: previous_height=None, header_hash=GENESIS_CHALLENGE, all-true
// filters (wallet_sync.rs:166-185). One page here: is_finished=true, height=the peak,
// header_hash=height_to_hash(peak), and the full spent+unspent history for the puzzle hash.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_puzzle_state_is_answered() {
    let (node, client, _push) = rig(true).await;
    let adds = common::additions();
    let watched = adds
        .iter()
        .find(|c| !c.coinbase)
        .map(|c| c.coin.puzzle_hash)
        .expect("a non-coinbase addition");
    let expected: Vec<Bytes32> = adds
        .iter()
        .filter(|c| c.coin.puzzle_hash == watched)
        .map(|c| c.coin.name())
        .collect();

    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestPuzzleState,
        &RequestPuzzleState {
            puzzle_hashes: vec![watched],
            previous_height: None,
            header_hash: MAINNET.genesis_challenge,
            filters: all_filters(),
            subscribe_when_finished: false,
        },
    )
    .await
    .expect("a RespondPuzzleState reply (request_puzzle_state is never silently dropped)");
    let resp: RespondPuzzleState = decode_reply(&reply, ProtocolMessageTypes::RespondPuzzleState);

    assert_eq!(
        resp.puzzle_hashes,
        vec![watched],
        "echoes the deduped request list"
    );
    assert!(
        resp.is_finished,
        "one page: everything fits the response budget"
    );
    assert_eq!(
        resp.height,
        common::PEAK_HEIGHT,
        "finished page reports the peak height"
    );
    assert_eq!(
        resp.header_hash,
        peak_header_hash(),
        "and the peak header hash"
    );
    let got: std::collections::HashSet<Bytes32> =
        resp.coin_states.iter().map(|cs| cs.coin.name()).collect();
    for name in &expected {
        assert!(
            got.contains(name),
            "every coin of the puzzle hash is served"
        );
    }
    assert_eq!(got.len(), expected.len(), "and nothing else");
    assert!(
        resp.coin_states
            .iter()
            .all(|cs| cs.created_height == Some(common::PEAK_HEIGHT)),
        "created heights carried"
    );
    drop(node);
}

// ── 1b. RequestCoinState answered (RED: timed out) ───────────────────────────────────────────────
// Sage fetch_coin/subscribe_coins shape: previous_height=None, header_hash=GENESIS_CHALLENGE
// (wallet_peer.rs:59/255). The response echoes the (deduped) coin id list and serves the states.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_coin_state_is_answered() {
    let (node, client, _push) = rig(true).await;
    let adds = common::additions();
    let coin = adds[0].coin;
    let missing = Bytes32::from([0xEE; 32]);

    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestCoinState,
        &RequestCoinState {
            // the in-request duplicate must be deduped in the echo
            coin_ids: vec![coin.name(), coin.name(), missing],
            previous_height: None,
            header_hash: MAINNET.genesis_challenge,
            subscribe: false,
        },
    )
    .await
    .expect("a RespondCoinState reply");
    let resp: RespondCoinState = decode_reply(&reply, ProtocolMessageTypes::RespondCoinState);

    assert_eq!(
        resp.coin_ids,
        vec![coin.name(), missing],
        "echoes the deduped, order-preserved id list"
    );
    assert_eq!(resp.coin_states.len(), 1, "only the known coin has a state");
    assert_eq!(resp.coin_states[0].coin.name(), coin.name());
    assert_eq!(
        resp.coin_states[0].created_height,
        Some(common::PEAK_HEIGHT)
    );
    drop(node);
}

// ── 1c. Remove-subscription requests answered (RED: timed out) ───────────────────────────────────
// Sage `unsubscribe()` sends remove_puzzle_subscriptions(None) + remove_coin_subscriptions(None)
// (wallet_peer.rs:225-238) and blocks on both replies. With nothing subscribed the removed set is
// empty — but the REPLY must still come.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remove_subscription_requests_are_answered() {
    let (node, client, _push) = rig(true).await;

    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestRemovePuzzleSubscriptions,
        &RequestRemovePuzzleSubscriptions {
            puzzle_hashes: None,
        },
    )
    .await
    .expect("a RespondRemovePuzzleSubscriptions reply");
    let resp: RespondRemovePuzzleSubscriptions = decode_reply(
        &reply,
        ProtocolMessageTypes::RespondRemovePuzzleSubscriptions,
    );
    assert!(resp.puzzle_hashes.is_empty(), "nothing was subscribed");

    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestRemoveCoinSubscriptions,
        &RequestRemoveCoinSubscriptions { coin_ids: None },
    )
    .await
    .expect("a RespondRemoveCoinSubscriptions reply");
    let resp: RespondRemoveCoinSubscriptions =
        decode_reply(&reply, ProtocolMessageTypes::RespondRemoveCoinSubscriptions);
    assert!(resp.coin_ids.is_empty());
    drop(node);
}

// ── 2. Reorg-consistency: header_hash must equal height_to_hash(previous_height)
// (GENESIS_CHALLENGE when previous_height=None); a mismatch is the client's chain forked from ours
// → RejectStateReason::REORG. A matching previous peak serves (empty page above min_height).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mismatched_previous_header_hash_rejects_reorg() {
    let (node, client, _push) = rig(true).await;
    let bogus = Bytes32::from([0x66; 32]);

    // puzzle-state: wrong hash at a known height → REORG reject (reason streamed as uint8).
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestPuzzleState,
        &RequestPuzzleState {
            puzzle_hashes: vec![Bytes32::from([0x01; 32])],
            previous_height: Some(common::PEAK_HEIGHT),
            header_hash: bogus,
            filters: all_filters(),
            subscribe_when_finished: false,
        },
    )
    .await
    .expect("a RejectPuzzleState reply");
    let rej: RejectPuzzleState = decode_reply(&reply, ProtocolMessageTypes::RejectPuzzleState);
    assert_eq!(rej.reason, RejectStateReason::REORG as u8);

    // coin-state: same check, RejectCoinState shape.
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestCoinState,
        &RequestCoinState {
            coin_ids: vec![Bytes32::from([0x02; 32])],
            previous_height: Some(common::PEAK_HEIGHT),
            header_hash: bogus,
            subscribe: false,
        },
    )
    .await
    .expect("a RejectCoinState reply");
    let rej: RejectCoinState = decode_reply(&reply, ProtocolMessageTypes::RejectCoinState);
    assert_eq!(rej.reason, RejectStateReason::REORG);

    // wrong hash at previous_height=None (must be the genesis challenge) → REORG too.
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestPuzzleState,
        &RequestPuzzleState {
            puzzle_hashes: vec![Bytes32::from([0x01; 32])],
            previous_height: None,
            header_hash: bogus,
            filters: all_filters(),
            subscribe_when_finished: false,
        },
    )
    .await
    .expect("a reply");
    let rej: RejectPuzzleState = decode_reply(&reply, ProtocolMessageTypes::RejectPuzzleState);
    assert_eq!(rej.reason, RejectStateReason::REORG as u8);

    // the MATCHING previous peak serves: min_height = peak+1 → an empty, finished page.
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestPuzzleState,
        &RequestPuzzleState {
            puzzle_hashes: vec![Bytes32::from([0x01; 32])],
            previous_height: Some(common::PEAK_HEIGHT),
            header_hash: peak_header_hash(),
            filters: all_filters(),
            subscribe_when_finished: false,
        },
    )
    .await
    .expect("a RespondPuzzleState reply");
    let resp: RespondPuzzleState = decode_reply(&reply, ProtocolMessageTypes::RespondPuzzleState);
    assert!(resp.is_finished);
    assert!(
        resp.coin_states.is_empty(),
        "nothing above the previous peak"
    );
    assert_eq!(resp.height, common::PEAK_HEIGHT);
    drop(node);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsynced_node_still_serves_puzzle_state() {
    let (node, client, _push) = rig(false).await;
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestPuzzleState,
        &RequestPuzzleState {
            puzzle_hashes: vec![Bytes32::from([0x01; 32])],
            previous_height: None,
            header_hash: MAINNET.genesis_challenge,
            filters: all_filters(),
            subscribe_when_finished: false,
        },
    )
    .await
    .expect("served while not synced (no sync gate here)");
    let resp: RespondPuzzleState = decode_reply(&reply, ProtocolMessageTypes::RespondPuzzleState);
    assert!(resp.is_finished);
    drop(node);
}

// ── 4. The connect greeting: `on_connect` sends the current peak as
// NewPeakWallet (fork_point_with_previous_peak = the peak height) to a WALLET-type peer. Sage
// gives a fresh peer 2 SECONDS to produce exactly this message before dropping it
// (peer_discovery.rs try_add_peer, options.rs initial_peak) — the assert budget below IS Sage's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wallet_handshake_is_greeted_with_new_peak_wallet_within_sage_budget() {
    let (_node, _client, mut push) = rig(true).await;
    let msg = tokio::time::timeout(Duration::from_secs(2), push.recv())
        .await
        .expect("NewPeakWallet within Sage's 2s initial_peak budget")
        .expect("push channel open");
    assert_eq!(msg.msg_type, ProtocolMessageTypes::NewPeakWallet);
    let peak: NewPeakWallet = decode_reply(&msg, ProtocolMessageTypes::NewPeakWallet);
    let rec = common::peak_record();
    assert_eq!(peak.header_hash, rec.header_hash);
    assert_eq!(peak.height, common::PEAK_HEIGHT);
    assert_eq!(peak.weight, rec.weight);
    assert_eq!(
        peak.fork_point_with_previous_peak,
        common::PEAK_HEIGHT,
        "on connect fork_point = the peak height itself"
    );
}

// ── 5. Peak advance → every wallet-type peer receives NewPeakWallet (broadcast after the
// per-subscriber CoinStateUpdate deltas).
// A full-node-type inbound peer must NOT receive it. The advance is a REAL confirm: the node
// syncs mainnet block 5,000,000 from a loopback peer through the production follow path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peak_advance_broadcasts_new_peak_wallet_to_wallet_peers() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // A loopback peer serving the fixture block.
    let block = common::full_block();
    let peer_api = Arc::new(common::MapApi {
        blocks: RwLock::new(HashMap::from([(common::PEAK_HEIGHT, block.clone())])),
    });
    let (peer_port, _peer_run) = common::spawn_serving_node(peer_api).await;

    // The node under test: EMPTY store (no on-connect peak greeting), peer server up.
    let listen = free_addr();
    let node = Arc::new(
        FullNode::boot(config(listen, free_addr()))
            .await
            .expect("boot"),
    );
    let (server, run, _peers) = node.build_peer_server().expect("peer server");
    tokio::spawn(async move { server.run(run).await });
    tokio::time::sleep(Duration::from_millis(150)).await;

    // A wallet-type peer and a full-node-type peer, both connected inbound.
    let (wallet_handlers, mut wallet_push) = push_capture_handlers();
    let _wallet = dial_wallet(listen.port(), wallet_handlers).await;
    let (fn_handlers, mut fn_push) = push_capture_handlers();
    let fn_run = Arc::new(AtomicBool::new(true));
    let _full_node_peer = dg_xch_p2p::dial(
        "127.0.0.1",
        listen.port(),
        fn_handlers,
        fn_run,
        &dg_xch_p2p::P2pSettings::default(),
    )
    .await
    .expect("full-node dial");

    // No peak yet → nothing greeted.
    assert!(wallet_push.try_recv().is_err(), "no peak, no greeting");

    // Confirm the block through the production follow path.
    let source = common::dial_source("127.0.0.1", peer_port).await;
    node.sync_follow(&source, common::PEAK_HEIGHT, common::PEAK_HEIGHT)
        .await
        .expect("sync");

    let msg = tokio::time::timeout(Duration::from_secs(5), wallet_push.recv())
        .await
        .expect("NewPeakWallet broadcast on peak advance")
        .expect("push channel open");
    assert_eq!(msg.msg_type, ProtocolMessageTypes::NewPeakWallet);
    let peak: NewPeakWallet = decode_reply(&msg, ProtocolMessageTypes::NewPeakWallet);
    let rec = common::peak_record();
    assert_eq!(peak.header_hash, rec.header_hash);
    assert_eq!(peak.height, common::PEAK_HEIGHT);
    assert_eq!(peak.weight, rec.weight);
    assert_eq!(
        peak.fork_point_with_previous_peak,
        common::PEAK_HEIGHT - 1,
        "a plain one-block advance forks at height-1"
    );
    assert!(
        fn_push.try_recv().is_err(),
        "NewPeakWallet goes to NodeType::Wallet peers only"
    );
}

// ── 6. THE ACCEPTANCE: a replay of Sage's exact sync sequence (sage-wallet wallet_sync.rs +
// wallet_peer.rs — the real sage crates carry their own TLS/peer stack, so the sequence is
// replayed message-for-message rather than linking them):
//
//   connect as WALLET → await NewPeakWallet (2s, try_add_peer)
//   → RequestCoinState(subscription coin ids, None, GENESIS, subscribe=true)      [sync_coin_ids]
//   → loop RequestPuzzleState(p2 hashes, prev, filters(true,true,true,0), true)   [sync_puzzle_hashes]
//     until is_finished, threading (height, header_hash) into the next request
//   → a fresh peak pushes CoinStateUpdate to the live subscription
//   → unsubscribe(): RemovePuzzleSubscriptions(None) + RemoveCoinSubscriptions(None)
//   → a further peak pushes NOTHING (subscriptions gone)
//
// Seeded state includes a CAT-shaped coin: its own puzzle hash is NOT the wallet's, only its
// HINT is — include_hinted=true must surface it (the hint join), exactly how Sage finds CATs/NFTs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sage_sync_sequence_end_to_end() {
    let (node, client, mut push) = rig(true).await;

    // Sage's step 0: the peer is only kept if NewPeakWallet arrives within 2s of connect.
    let greeted = tokio::time::timeout(Duration::from_secs(2), push.recv())
        .await
        .expect("NewPeakWallet within Sage's keep-alive budget")
        .expect("open");
    assert_eq!(greeted.msg_type, ProtocolMessageTypes::NewPeakWallet);

    // The wallet's world: one p2 puzzle hash with an unspent coin and a spent coin, plus a
    // CAT-shaped coin hinted at the p2 hash.
    let p2 = Bytes32::from([0xA7; 32]);
    let unspent = Coin {
        parent_coin_info: Bytes32::from([0x01; 32]),
        puzzle_hash: p2,
        amount: 1_000,
    };
    let spent = Coin {
        parent_coin_info: Bytes32::from([0x02; 32]),
        puzzle_hash: p2,
        amount: 2_000,
    };
    let cat = Coin {
        parent_coin_info: Bytes32::from([0x03; 32]),
        puzzle_hash: Bytes32::from([0xCA; 32]), // NOT the wallet's hash
        amount: 3_000,
    };
    let rec = |c: Coin| CoinRecord {
        coin: c,
        confirmed_block_index: common::PEAK_HEIGHT,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    };
    node.store
        .apply_block(
            common::PEAK_HEIGHT,
            0,
            &[rec(unspent), rec(spent), rec(cat)],
            &[],
        )
        .await
        .expect("seed coins");
    node.store
        .apply_block(common::PEAK_HEIGHT, 0, &[], &[spent.name()])
        .await
        .expect("spend one");
    node.store
        .apply_hints(&[(p2, cat.name())])
        .await
        .expect("hint the CAT at the wallet's p2 hash");

    // [sync_coin_ids] — subscribe_coins(coin_ids, None, genesis) (wallet_sync.rs:137).
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestCoinState,
        &RequestCoinState {
            coin_ids: vec![unspent.name()],
            previous_height: None,
            header_hash: MAINNET.genesis_challenge,
            subscribe: true,
        },
    )
    .await
    .expect("RespondCoinState");
    let resp: RespondCoinState = decode_reply(&reply, ProtocolMessageTypes::RespondCoinState);
    assert_eq!(resp.coin_states.len(), 1);
    assert_eq!(resp.coin_states[0].coin.name(), unspent.name());

    // [sync_puzzle_hashes] — the paging loop (wallet_sync.rs:169-206).
    let mut prev_height: Option<u32> = None;
    let mut prev_header_hash = MAINNET.genesis_challenge;
    let mut synced: Vec<CoinState> = Vec::new();
    for _page in 0..10 {
        let reply = request(
            &client.connection,
            ProtocolMessageTypes::RequestPuzzleState,
            &RequestPuzzleState {
                puzzle_hashes: vec![p2],
                previous_height: prev_height,
                header_hash: prev_header_hash,
                filters: all_filters(),
                subscribe_when_finished: true,
            },
        )
        .await
        .expect("RespondPuzzleState");
        let resp: RespondPuzzleState =
            decode_reply(&reply, ProtocolMessageTypes::RespondPuzzleState);
        synced.extend(resp.coin_states.iter().cloned());
        prev_height = Some(resp.height);
        prev_header_hash = resp.header_hash;
        if resp.is_finished {
            break;
        }
    }
    let names: std::collections::HashSet<Bytes32> =
        synced.iter().map(|cs| cs.coin.name()).collect();
    assert_eq!(
        names,
        [unspent.name(), spent.name(), cat.name()]
            .into_iter()
            .collect(),
        "the sync converges to the seeded state: plain coins AND the hinted CAT"
    );
    let spent_state = synced
        .iter()
        .find(|cs| cs.coin.name() == spent.name())
        .expect("spent coin present");
    assert_eq!(spent_state.spent_height, Some(common::PEAK_HEIGHT));

    // The subscription is LIVE: a new peak touching the p2 hash pushes CoinStateUpdate.
    let fresh = Coin {
        parent_coin_info: Bytes32::from([0x04; 32]),
        puzzle_hash: p2,
        amount: 4_000,
    };
    let fresh_rec = CoinRecord {
        coin: fresh,
        confirmed_block_index: common::PEAK_HEIGHT + 1,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    };
    node.store
        .apply_block(
            common::PEAK_HEIGHT + 1,
            0,
            std::slice::from_ref(&fresh_rec),
            &[],
        )
        .await
        .expect("fresh block");
    node.wallet
        .on_new_peak(
            node.store.as_ref(),
            WalletUpdate {
                peak_hash: Bytes32::from([0xF1; 32]),
                height: common::PEAK_HEIGHT + 1,
                fork_height: common::PEAK_HEIGHT,
                created: std::slice::from_ref(&fresh_rec),
                spent_ids: &[],
                hints: &[],
            },
        )
        .await
        .expect("push");
    let update = tokio::time::timeout(Duration::from_secs(5), push.recv())
        .await
        .expect("CoinStateUpdate for the subscribed puzzle hash")
        .expect("open");
    assert_eq!(update.msg_type, ProtocolMessageTypes::CoinStateUpdate);
    let update: CoinStateUpdate = decode_reply(&update, ProtocolMessageTypes::CoinStateUpdate);
    assert_eq!(update.height, common::PEAK_HEIGHT + 1);
    assert!(update.items.iter().any(|cs| cs.coin.name() == fresh.name()));

    // [unsubscribe] — remove-all returns exactly what was subscribed (clear + return the
    // prior set).
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestRemovePuzzleSubscriptions,
        &RequestRemovePuzzleSubscriptions {
            puzzle_hashes: None,
        },
    )
    .await
    .expect("RespondRemovePuzzleSubscriptions");
    let resp: RespondRemovePuzzleSubscriptions = decode_reply(
        &reply,
        ProtocolMessageTypes::RespondRemovePuzzleSubscriptions,
    );
    assert_eq!(resp.puzzle_hashes, vec![p2]);
    let reply = request(
        &client.connection,
        ProtocolMessageTypes::RequestRemoveCoinSubscriptions,
        &RequestRemoveCoinSubscriptions { coin_ids: None },
    )
    .await
    .expect("RespondRemoveCoinSubscriptions");
    let resp: RespondRemoveCoinSubscriptions =
        decode_reply(&reply, ProtocolMessageTypes::RespondRemoveCoinSubscriptions);
    assert_eq!(resp.coin_ids, vec![unspent.name()]);

    // Subscriptions gone: a further matching peak pushes nothing.
    node.wallet
        .on_new_peak(
            node.store.as_ref(),
            WalletUpdate {
                peak_hash: Bytes32::from([0xF2; 32]),
                height: common::PEAK_HEIGHT + 2,
                fork_height: common::PEAK_HEIGHT + 1,
                created: std::slice::from_ref(&fresh_rec),
                spent_ids: &[unspent.name()],
                hints: &[],
            },
        )
        .await
        .expect("push after unsubscribe");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        push.try_recv().is_err(),
        "no CoinStateUpdate after remove-all unsubscribe"
    );
}

// (The over-cap ExceededSubscriptionLimit reject and the paging/filter matrix are proven at the
// API level in `node/test_suites.rs`, where the 100k response budget and 200k subscription
// cap are injectable because seeding 200k live subscriptions over the wire is impractical.
// additionally truncates the request LISTS at parse time via its `list_limits` decorator
// (→ parse_list_limited); our handlers apply the identical
// truncation after decode, which is byte-stream- and semantics-equivalent.)
