#![allow(dead_code)]

use dg_full_node::FullNode;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_with_args::ConditionWithArgs;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use portfu::prelude::{ServerBuilder, ServerHandle};
use serde::Deserialize;
use std::sync::atomic::{AtomicU64, Ordering};

pub const PEAK_HEIGHT: u32 = 5_000_000;
static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct AddsRems {
    additions: Vec<CoinRecord>,
    removals: Vec<CoinRecord>,
}

#[must_use]
pub fn records() -> Vec<BlockRecord> {
    serde_json::from_str(include_str!("../fixtures/block_records.json")).expect("records fixture")
}

#[must_use]
pub fn full_block() -> FullBlock {
    serde_json::from_str(include_str!("../fixtures/full_block_5000000.json"))
        .expect("block fixture")
}

#[must_use]
pub fn peak_record() -> BlockRecord {
    records()
        .into_iter()
        .find(|r| r.height == PEAK_HEIGHT)
        .expect("peak record present")
}

#[must_use]
pub fn additions() -> Vec<CoinRecord> {
    let ar: AddsRems = serde_json::from_str(include_str!("../fixtures/adds_rems_5000000.json"))
        .expect("adds fixture");
    ar.additions
}

pub async fn open_store() -> SqliteStore {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("full_node_test_{}_{n}.sqlite", std::process::id()));
    SqliteStore::open(&path).await.expect("open store")
}

pub async fn spawn_portfu_rpc(
    node: &std::sync::Arc<FullNode<SqliteStore>>,
    bind: std::net::SocketAddr,
) -> ServerHandle {
    let tls =
        dg_full_node::build_portfu_rpc_tls_context(&node.config.rpc_tls).expect("Portfu RPC TLS");
    node.attach_rpc_live(tls.node_id);
    let server = ServerBuilder::new()
        .host(bind.ip().to_string())
        .port(bind.port())
        .tls(tls.tls_config)
        .shutdown_grace_period(std::time::Duration::from_millis(100))
        .global_state::<dg_full_node::Node>(node.state.clone())
        .build();
    let handle = server.handle();
    tokio::spawn(async move {
        let _ = server.run().await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    handle
}

// Seed a store to the real mainnet block 5000000: candidate record + cold body + confirmed peak + the
// block's real addition coins (unspent). The RPC/wallet read paths then run against real data.
pub async fn seed_peak(store: &SqliteStore) -> (BlockRecord, FullBlock, Vec<CoinRecord>) {
    let rec = peak_record();
    let block = full_block();
    assert_eq!(
        rec.header_hash,
        block.header_hash().expect("hh"),
        "fixture record must match its block"
    );
    store
        .add_block_records(std::slice::from_ref(&rec))
        .await
        .expect("records");
    let mut batch = store.begin().await.expect("begin");
    store
        .append_many(&mut batch, std::slice::from_ref(&block))
        .await
        .expect("append");
    store.commit(batch).await.expect("commit");
    store.set_peak(&rec.header_hash).await.expect("set peak");
    let adds = additions();
    store
        .apply_block(PEAK_HEIGHT, rec.timestamp.unwrap_or(0), &adds, &[])
        .await
        .expect("apply coins");
    (rec, block, adds)
}

// ---- spend-bundle builders for push_tx (a bundle spending one already-unspent coin) --------------------

fn h(tag: u8) -> Bytes32 {
    Bytes32::from([tag; 32])
}

#[must_use]
pub fn bundle(sig_tag: u8) -> SpendBundle {
    SpendBundle {
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([sig_tag; 96]),
    }
}

// ---- a real runnable bundle for the server-side push_tx path -------------------------------------------
// The `1` puzzle echoes its solution as the condition list, so the bundle passes the full CLVM run +
// puzzle-hash binding; the infinity signature satisfies the aggregate check (no AGG_SIG conditions).

#[must_use]
pub fn easy_puzzle_hash() -> Bytes32 {
    Program::to(1_u8).tree_hash()
}

// Seed an unspent coin locked by the easy puzzle at the peak height, so a bundle spending it is
// mempool-admissible against the confirmed set.
pub async fn seed_easy_coin<S: CoinStore + ?Sized>(store: &S, amount: u64) -> Coin {
    let coin = Coin {
        parent_coin_info: h(0xEC),
        puzzle_hash: easy_puzzle_hash(),
        amount,
    };
    let rec = CoinRecord {
        coin,
        coinbase: false,
        confirmed_block_index: PEAK_HEIGHT,
        spent: false,
        spent_block_index: 0,
        timestamp: 0,
    };
    store
        .apply_block(PEAK_HEIGHT, 0, std::slice::from_ref(&rec), &[])
        .await
        .expect("apply easy coin");
    coin
}

// A bundle spending `coin` through the easy puzzle with a single RESERVE_FEE condition.
#[must_use]
pub fn easy_bundle(coin: &Coin, fee: u64) -> SpendBundle {
    conditioned_bundle(coin, &[ConditionWithArgs::ReserveFee(fee)])
}

// A bundle spending `coin` through the easy puzzle with an arbitrary condition list — the
// timelock/assert cases of the SendTransaction ack tests build on this.
#[must_use]
pub fn conditioned_bundle(coin: &Coin, conditions: &[ConditionWithArgs]) -> SpendBundle {
    let puzzle = Program::to(1_u8);
    let solution = Program::to(
        conditions
            .iter()
            .map(|c| SExp::from(c).to_owned())
            .collect::<Vec<_>>(),
    );
    let mut infinity = [0_u8; 96];
    infinity[0] = 0xc0;
    SpendBundle {
        coin_spends: vec![CoinSpend {
            coin: *coin,
            puzzle_reveal: puzzle.serialized().expect("puzzle serializes"),
            solution: solution.serialized().expect("solution serializes"),
        }],
        aggregated_signature: Bytes96::from(infinity),
    }
}

// ---- loopback peer harness (a second node we sync from / pull blocks from) ------------------------------

use async_trait::async_trait;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_node::sync::source::{BlockRangeSource, OutboundPeerSource};
use dg_xch_p2p::{FullNodeApi, OutboundPeer, P2pSettings};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use tokio::sync::RwLock;

// A peer that serves blocks from an in-memory height map (the loopback stand-in for a real full node).
pub struct MapApi {
    pub blocks: RwLock<HashMap<u32, FullBlock>>,
}

#[async_trait]
impl FullNodeApi for MapApi {
    async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        self.blocks.read().await.get(&height).cloned().map(Box::new)
    }
    async fn gossip_peers(&self) -> Vec<TimestampedPeerInfo> {
        Vec::new()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

pub async fn spawn_serving_node(api: Arc<MapApi>) -> (u16, Arc<AtomicBool>) {
    use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let port = free_port();
    let dyn_api: Arc<dyn FullNodeApi> = api;
    let handlers = dg_xch_p2p::full_node_handlers(dyn_api, "mainnet".to_string(), port);
    let handlers = Arc::new(RwLock::new(handlers));
    let peers: dg_xch_core::protocols::PeerMap = Arc::new(RwLock::new(HashMap::new()));
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

// Dial a peer server and wrap the outbound channel as a block-range source (the sync/serve seam).
pub async fn dial_source(host: &str, port: u16) -> Arc<dyn BlockRangeSource> {
    let settings = P2pSettings::default();
    let handlers = Arc::new(RwLock::new(HashMap::new()));
    let run = Arc::new(AtomicBool::new(true));
    let client = dg_xch_p2p::dial(host, port, handlers, run.clone(), &settings)
        .await
        .expect("dial peer");
    let peer = Arc::new(OutboundPeer {
        endpoint: (host.to_string(), port),
        client,
        run,
    });
    Arc::new(OutboundPeerSource::new(peer, Duration::from_secs(15)))
}
