#![allow(dead_code)]

pub mod ancestry;
pub mod fault;
pub mod lineage;
pub mod sweep;

use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_node::SyncError;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_stores::SqliteStore;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize)]
struct AddsRems {
    additions: Vec<CoinRecord>,
    removals: Vec<CoinRecord>,
}

#[must_use]
pub fn load_records() -> Vec<BlockRecord> {
    serde_json::from_str(include_str!("../fixtures/block_records.json")).expect("records fixture")
}

#[must_use]
pub fn load_full_block(height: u32) -> FullBlock {
    let raw = match height {
        5_000_000 => include_str!("../fixtures/full_block_5000000.json"),
        5_000_004 => include_str!("../fixtures/full_block_5000004.json"),
        other => panic!("no full-block fixture for height {other}"),
    };
    serde_json::from_str(raw).expect("full block fixture")
}

/// Real mainnet additions/removals for a transaction block; removals are returned as coin records.
#[must_use]
pub fn load_adds_rems(height: u32) -> (Vec<CoinRecord>, Vec<CoinRecord>) {
    let raw = match height {
        5_000_000 => include_str!("../fixtures/adds_rems_5000000.json"),
        5_000_004 => include_str!("../fixtures/adds_rems_5000004.json"),
        5_000_007 => include_str!("../fixtures/adds_rems_5000007.json"),
        5_000_012 => include_str!("../fixtures/adds_rems_5000012.json"),
        other => panic!("no adds/rems fixture for height {other}"),
    };
    let ar: AddsRems = serde_json::from_str(raw).expect("adds/rems fixture");
    (ar.additions, ar.removals)
}

#[must_use]
pub fn unique_db_path() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dg_xch_node_test_{}_{n}.sqlite",
        std::process::id()
    ))
}

pub async fn new_store() -> SqliteStore {
    SqliteStore::open(&unique_db_path())
        .await
        .expect("open store")
}

const RESTAMP_BASE_WEIGHT: u128 = 1_000_000;

#[must_use]
pub fn synth_hash(tag: u8, h: u32) -> Bytes32 {
    let mut b = [tag; 32];
    b[28..32].copy_from_slice(&h.to_be_bytes());
    Bytes32::from(b)
}

// Re-stamp a real mainnet body onto a synthetic contiguous height: a distinct foliage.prev_block_hash gives a
// distinct header_hash per height (so bodies key distinctly in the store); height/weight link a candidate
// chain. The generator/coins are untouched — full body validation stays real under add_block.
#[must_use]
pub fn restamp_block(base: &FullBlock, h: u32) -> FullBlock {
    let mut b = base.clone();
    b.reward_chain_block.height = h;
    b.reward_chain_block.weight = RESTAMP_BASE_WEIGHT + u128::from(h);
    b.foliage.prev_block_hash = synth_hash(0xcd, h.saturating_sub(1));
    b
}

// The candidate record for a re-stamped height: its header_hash matches restamp_block(h) so the write-through
// body associates, and get_unassociated reports the height until the body lands.
#[must_use]
pub fn candidate_record(template: &BlockRecord, base: &FullBlock, h: u32) -> BlockRecord {
    let mut r = template.clone();
    r.header_hash = restamp_block(base, h).header_hash().expect("header hash");
    r.prev_hash = synth_hash(0xcd, h.saturating_sub(1));
    r.height = h;
    r.weight = RESTAMP_BASE_WEIGHT + u128::from(h);
    r.total_iters = RESTAMP_BASE_WEIGHT + u128::from(h);
    r.timestamp = Some(1_700_000_000 + u64::from(h));
    r.sub_epoch_summary_included = None;
    r
}

// An in-memory peer that serves re-stamped bodies for any requested height — the loopback stand-in that lets
// the window/write-through/reclaim machinery run at any range size without a socket.
pub struct MemSource {
    pub id: u64,
    pub base: FullBlock,
}

#[async_trait]
impl BlockRangeSource for MemSource {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        Ok((start..=end)
            .map(|h| restamp_block(&self.base, h))
            .collect())
    }
}

// A peer that accepts a reservation and never answers — the stalled-peer case. Its reservation must be
// reclaimed to the pool and finished by a live peer.
pub struct StallingSource {
    pub id: u64,
}

#[async_trait]
impl BlockRangeSource for StallingSource {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, _start: u32, _end: u32) -> Result<Vec<FullBlock>, SyncError> {
        std::future::pending::<()>().await;
        unreachable!()
    }
}

// A mutable full-node API — node A. Its served block set can be advanced mid-test so node B has a new peak
// to follow (short sync).
pub struct LiveNodeApi {
    pub blocks: tokio::sync::RwLock<std::collections::HashMap<u32, FullBlock>>,
}

#[async_trait]
impl dg_xch_p2p::FullNodeApi for LiveNodeApi {
    async fn block_by_height(&self, height: u32) -> Option<Box<FullBlock>> {
        self.blocks.read().await.get(&height).cloned().map(Box::new)
    }
    async fn gossip_peers(&self) -> Vec<dg_xch_core::blockchain::peer_info::TimestampedPeerInfo> {
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

pub async fn spawn_node_a(
    api: std::sync::Arc<LiveNodeApi>,
) -> (u16, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use dg_xch_servers::websocket::{WebsocketServer, WebsocketServerConfig};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let port = free_port();
    let dyn_api: std::sync::Arc<dyn dg_xch_p2p::FullNodeApi> = api;
    let handlers_map = dg_xch_p2p::full_node_handlers(dyn_api, "mainnet".to_string(), port);
    let handlers = std::sync::Arc::new(tokio::sync::RwLock::new(handlers_map));
    let peers: dg_xch_core::protocols::PeerMap =
        std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let config = WebsocketServerConfig {
        host: "127.0.0.1".to_string(),
        port,
        ssl_info: None,
    };
    let server = WebsocketServer::new(&config, peers, handlers).expect("server");
    let run = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let run_c = run.clone();
    tokio::spawn(async move {
        let _ = server.run(run_c).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    (port, run)
}

// Node B dials node A and wraps the outbound channel as a block-range source.
pub async fn dial_source(port: u16) -> std::sync::Arc<dyn BlockRangeSource> {
    use dg_xch_node::sync::source::OutboundPeerSource;
    let settings = dg_xch_p2p::P2pSettings::default();
    let handlers = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let run = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let client = dg_xch_p2p::dial("127.0.0.1", port, handlers, run.clone(), &settings)
        .await
        .expect("dial node A");
    let peer = std::sync::Arc::new(dg_xch_p2p::OutboundPeer {
        endpoint: ("127.0.0.1".to_string(), port),
        client,
        run,
    });
    std::sync::Arc::new(OutboundPeerSource::new(
        peer,
        std::time::Duration::from_secs(15),
    ))
}

// A candidate header record for a real block (header_hash matches so the write-through body associates).
#[must_use]
pub fn seed_record_for(template: &BlockRecord, block: &FullBlock) -> BlockRecord {
    let mut rec = template.clone();
    rec.header_hash = block.header_hash().expect("hh");
    rec.prev_hash = block.prev_header_hash();
    rec.height = block.height();
    rec.weight = block.reward_chain_block.weight;
    rec.total_iters = block.reward_chain_block.weight;
    rec.sub_epoch_summary_included = None;
    rec
}

// Process peak resident set size. macOS reports ru_maxrss in bytes, Linux in KiB — normalize to bytes.
#[must_use]
pub fn peak_rss_bytes() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    assert_eq!(rc, 0, "getrusage");
    let maxrss = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") {
        maxrss
    } else {
        maxrss * 1024
    }
}

/// Adversarial-peer misbehavior modes (test class 4). `NeverReply` is the block-range analog of the
/// never-draining websocket sink from the core timeout-send unit (`send_timeout_tests`): the
/// request is accepted and the reply future stays `Pending` forever — the exact backpressure shape
/// that used to wedge the follow loop before the bounded send.
#[derive(Clone, Copy, Debug)]
pub enum Misbehavior {
    /// Accepts every request and never answers.
    NeverReply,
    /// Rejects every requested range (`RejectBlocks` on everything).
    RejectAllRanges,
    /// Honest but outdated: serves ranges wholly at or below its stale tip, rejects anything above
    /// (a peer answers `RequestBlocks` past its peak with `RejectBlocks`).
    StalePeak { tip: u32 },
    /// Serves the first half of its first range, then the connection drops: every later fetch
    /// errors and `is_closed` reports the drop.
    DisconnectMidWindow,
}

/// A peer that misbehaves in one fixed [`Misbehavior`] mode. The ranges it does serve honestly come
/// from `serve` when the height is present (real fixture bodies), else re-stamped bodies from
/// `base` (the `MemSource` convention for synthetic ranges).
pub struct MisbehavingSource {
    pub id: u64,
    pub mode: Misbehavior,
    pub base: FullBlock,
    pub serve: std::collections::HashMap<u32, FullBlock>,
    pub fetches: AtomicU64,
    pub closed: AtomicBool,
}

impl MisbehavingSource {
    #[must_use]
    pub fn new(id: u64, mode: Misbehavior, base: FullBlock) -> Self {
        Self {
            id,
            mode,
            base,
            serve: std::collections::HashMap::new(),
            fetches: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Real bodies for the heights this peer serves honestly (its pre-misbehavior half).
    #[must_use]
    pub fn with_serve(mut self, serve: std::collections::HashMap<u32, FullBlock>) -> Self {
        self.serve = serve;
        self
    }

    fn body(&self, h: u32) -> FullBlock {
        self.serve
            .get(&h)
            .cloned()
            .unwrap_or_else(|| restamp_block(&self.base, h))
    }
}

#[async_trait]
impl BlockRangeSource for MisbehavingSource {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        let n = self.fetches.fetch_add(1, Ordering::Relaxed);
        match self.mode {
            Misbehavior::NeverReply => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            Misbehavior::RejectAllRanges => Err(SyncError::RangeRejected { start, end }),
            Misbehavior::StalePeak { tip } => {
                if end <= tip {
                    Ok((start..=end).map(|h| self.body(h)).collect())
                } else {
                    Err(SyncError::RangeRejected { start, end })
                }
            }
            Misbehavior::DisconnectMidWindow => {
                if n == 0 {
                    // Honest-then-drop: the first request is served (the first half of the
                    // range), then the connection dies mid-window.
                    self.closed.store(true, Ordering::Relaxed);
                    let mid = start + (end - start) / 2;
                    Ok((start..=mid).map(|h| self.body(h)).collect())
                } else {
                    Err(SyncError::Io(std::io::Error::other(
                        "connection closed mid-window",
                    )))
                }
            }
        }
    }
}

/// A good in-memory peer serving exact fixture blocks by height; a range it cannot fully serve is
/// rejected (the honest `RejectBlocks` answer).
pub struct FixtureSource {
    pub id: u64,
    pub blocks: std::collections::HashMap<u32, FullBlock>,
}

#[async_trait]
impl BlockRangeSource for FixtureSource {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        (start..=end)
            .map(|h| {
                self.blocks
                    .get(&h)
                    .cloned()
                    .ok_or(SyncError::RangeRejected { start, end })
            })
            .collect()
    }
}
