// Reorg ACROSS the offline gap. A node with history comes back after weeks; the weight proof's
// fork point lands BELOW the local peak (the network reorged deeper than our tip while we were
// away). The node re-follows forward windows from the fork point through
// `Chaser::long_sync_reland_reporting`, staging the divergent branch as orphan candidates until
// it outweighs the stale peak — at which point the engine's single-transaction reorg flips the
// chain. The short backtrack cannot cross this gap (its cap is 5 — pinned in backtrack.rs
// `fork_deeper_than_the_backtrack_cap...`).
//
// Fixture conventions follow backtrack.rs: re-stamped real mainnet bodies on synthetic
// (height, weight, prev) links, assume_valid above every synthetic height.

mod common;

use async_trait::async_trait;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig, SyncError};
use dg_xch_stores::BlockStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE_WEIGHT: u128 = 1_000_000;
const FORK: u32 = 110; // WP fork point: 35 below our peak — far past the backtrack cap (5)
const A_TIP: u32 = 145; // our stale confirmed peak (offline since here)
const B_TIP: u32 = 160; // the network's chain, reorged at FORK while we were away

fn link_block(base: &FullBlock, h: u32, weight: u128, prev: Bytes32) -> FullBlock {
    let mut b = base.clone();
    b.reward_chain_block.height = h;
    b.reward_chain_block.weight = weight;
    b.foliage.prev_block_hash = prev;
    b
}

fn build_chain(
    base: &FullBlock,
    start: u32,
    end: u32,
    prev0: Bytes32,
    weight_at: impl Fn(u32) -> u128,
) -> Vec<FullBlock> {
    let mut prev = prev0;
    let mut out = Vec::new();
    for h in start..=end {
        let b = link_block(base, h, weight_at(h), prev);
        prev = b.header_hash().expect("header hash");
        out.push(b);
    }
    out
}

fn a_weight(h: u32) -> u128 {
    BASE_WEIGHT + u128::from(h - 100) * 10
}

// Branch B gains 12/block from the fork: B outweighs our stale A(145) peak (BASE+450) at
// B(140) (BASE+100+30*12 = BASE+460) — INSIDE the first reland window, while every B block up
// to 139 is lighter and must park as an orphan candidate.
fn b_weight(h: u32) -> u128 {
    a_weight(FORK) + u128::from(h - FORK) * 12
}

struct ForkedPeer {
    by_height: HashMap<u32, FullBlock>,
}

impl ForkedPeer {
    fn new(chain_a: &[FullBlock], fork: u32, chain_b: &[FullBlock]) -> Self {
        let mut by_height = HashMap::new();
        for b in chain_a.iter().filter(|b| b.height() <= fork) {
            by_height.insert(b.height(), b.clone());
        }
        for b in chain_b {
            by_height.insert(b.height(), b.clone());
        }
        Self { by_height }
    }
}

#[async_trait]
impl BlockRangeSource for ForkedPeer {
    fn peer_id(&self) -> u64 {
        0x62
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        Ok((start..=end)
            .filter_map(|h| self.by_height.get(&h).cloned())
            .collect())
    }
}

async fn chaser_on_branch_a(
    chain_a: &[FullBlock],
) -> Chaser<Arc<dg_xch_stores::SqliteStore>, NativePrimitives> {
    let store = Arc::new(common::new_store().await);
    let engine = Engine::new(store, NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(
        engine,
        SyncConfig {
            peers: 1,
            window: 4,
            batch: 32,
            request_timeout: Duration::from_secs(20),
            assume_valid: 10_000_000,
        },
    );
    let peak = chaser
        .follow_blocks(chain_a)
        .await
        .expect("branch A confirms");
    assert_eq!(
        peak,
        Some((chain_a.last().unwrap().header_hash().unwrap(), A_TIP)),
        "precondition: our stale peak is branch A's tip"
    );
    chaser
}

fn fixture_chains() -> (Vec<FullBlock>, Vec<FullBlock>) {
    let base_a = common::load_full_block(5_000_000);
    let base_b = common::load_full_block(5_000_004);
    let chain_a = build_chain(&base_a, 100, A_TIP, common::synth_hash(0xaa, 99), a_weight);
    let fork_hash = chain_a[(FORK - 100) as usize].header_hash().unwrap();
    let chain_b = build_chain(&base_b, FORK + 1, B_TIP, fork_hash, b_weight);
    (chain_a, chain_b)
}

// GREEN target — the reland rewinds THROUGH the engine's atomic reorg: re-following from the WP
// fork point stages the divergent branch as orphan candidates (all lighter than our stale peak
// until B(140)), then the single-transaction reorg flips the chain onto the heavier branch. The
// confirmed peak leaves the stale branch and the reorg-depth metric records the fork.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reland_reorgs_across_the_gap_onto_the_heavier_branch() {
    let (chain_a, chain_b) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, FORK, &chain_b));

    // The stale-tip wedge is real: a plain forward follow from peak+1 orphans (the branch
    // reorged 35 below us — no forward window can ever connect), exactly the pre-G2 grind.
    let err = chaser
        .follow_to(&peer, A_TIP + 1, B_TIP)
        .await
        .expect_err("forward follow across a reorged gap must orphan");
    assert!(err.is_orphan(), "expected the orphan wedge, got: {err}");

    let (peak, deltas) = chaser
        .long_sync_reland_reporting(&peer, FORK)
        .await
        .expect("reland must converge through the engine reorg");

    // The reland stops as soon as the peak leaves the stale branch: the first window
    // [FORK+1, FORK+32] carries B(140), the first block to outweigh A(145).
    let (peak_hash, peak_height) = peak.expect("a confirmed peak");
    let expect_height = FORK + 32; // window tip after the in-window reorg confirms through it
    assert_eq!(
        (peak_hash, peak_height),
        (
            chain_b[(expect_height - FORK - 1) as usize]
                .header_hash()
                .unwrap(),
            expect_height
        ),
        "the confirmed peak must land on branch B past the reorg"
    );
    // The atomic-reorg path (T0-4) ran, at the depth the WP fork point named: the reorging
    // block B(140) over fork height 110.
    assert_eq!(
        chaser.metrics().last_reorg_depth.load(Ordering::Relaxed),
        30,
        "reorg depth = reorging block height (140) - fork height (110)"
    );
    // The reorg deltas surfaced for the server's wallet/mempool feed.
    assert!(
        deltas.iter().any(|d| d.delta.height == 140),
        "the reorged branch's deltas must be reported"
    );
    // The store's confirmed chain is branch B at every shared height above the fork.
    let store = chaser.engine().store().clone();
    for b in chain_b
        .iter()
        .filter(|b| b.height() > FORK && b.height() <= expect_height)
    {
        let rec = store
            .get_block_record_by_height(b.height())
            .await
            .expect("record")
            .expect("in-chain record");
        assert_eq!(
            rec.header_hash,
            b.header_hash().unwrap(),
            "height {} must be branch B on the confirmed chain",
            b.height()
        );
    }
}

// The probe-failed-but-no-divergence shape: the fork point is conservative (below the peak) but
// the peer is on OUR chain. The reland re-follows from the fork point — every shared block
// confirms as AlreadyHave, then the peer's extension advances the peak normally. No reorg, no
// wedge, bounded work.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reland_over_our_own_chain_is_already_have_then_extends() {
    let (chain_a, _) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;
    // The peer agrees with us and has 5 more blocks.
    let base_a = common::load_full_block(5_000_000);
    let ext = build_chain(
        &base_a,
        A_TIP + 1,
        A_TIP + 5,
        chain_a.last().unwrap().header_hash().unwrap(),
        a_weight,
    );
    let mut full = chain_a.clone();
    full.extend(ext.clone());
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&full, A_TIP + 5, &[]));

    let (peak, _) = chaser
        .long_sync_reland_reporting(&peer, A_TIP - 5)
        .await
        .expect("reland over our own chain must extend");
    assert_eq!(
        peak,
        Some((ext.last().unwrap().header_hash().unwrap(), A_TIP + 5)),
        "AlreadyHave through our blocks, then the extension advances the peak"
    );
    assert_eq!(
        chaser.metrics().last_reorg_depth.load(Ordering::Relaxed),
        0,
        "no reorg on an agreeing chain"
    );
}
