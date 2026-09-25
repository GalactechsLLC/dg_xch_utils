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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

// The torn-peak shape (a real mainnet reorg case, height 6,623,218): a pre-T0-4 crash left the store's confirmed PEAK
// itself on the wrong branch — the block AT the peak height differs from the canonical chain's block
// at the SAME height, with EQUAL weight (a same-height sibling, the realistic mainnet orphan), and
// the canonical chain continues above it. This is the fork-at-exactly-peak-minus-1 boundary of the
// short-sync backtrack: the very first backtracked height (depth 0 = the peak height itself) must
// find the fork, and the engine's fork choice must FLIP the peak through the atomic reorg
// the moment the canonical branch outweighs — never "converged past the fork" without moving
// the peak. Distinct from backtrack.rs, whose fork sits 2 below the peak and never exercises
// the same-height-sibling boundary.

const BASE_WEIGHT: u128 = 1_000_000;
const FORK: u32 = 105; // last common block — exactly peak - 1
const PEAK: u32 = 106; // our confirmed peak: the WRONG (non-canonical) block X at this height
const B_TIP: u32 = 110; // the canonical peer's tip

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

// One weight schedule for BOTH branches: X and Y carry the SAME weight at PEAK (same-height mainnet
// siblings differ in body, not weight), so canonical Y alone can never outweigh X — only its
// child at PEAK+1 flips the branch.
fn weight_at(h: u32) -> u128 {
    BASE_WEIGHT + u128::from(h - 100) * 10
}

/// Our chain: shared history 100..=FORK, then the wrong sibling X at PEAK.
/// The canonical chain: the same shared history, then Y at PEAK (equal weight, different body →
/// different hash) continuing to B_TIP.
fn fixture_chains() -> (Vec<FullBlock>, Vec<FullBlock>) {
    let base_shared = common::load_full_block(5_000_000);
    let base_canonical = common::load_full_block(5_000_004);
    let shared = build_chain(
        &base_shared,
        100,
        FORK,
        common::synth_hash(0xaa, 99),
        weight_at,
    );
    let fork_hash = shared.last().unwrap().header_hash().unwrap();
    // X: one more block re-stamped from the SHARED base body → hash differs from Y's at PEAK.
    let wrong_peak = link_block(&base_shared, PEAK, weight_at(PEAK), fork_hash);
    let mut ours = shared.clone();
    ours.push(wrong_peak);
    // Canonical: Y at PEAK from the OTHER base body, then B_TIP - PEAK more blocks on top.
    let canonical_above = build_chain(&base_canonical, PEAK, B_TIP, fork_hash, weight_at);
    let mut canonical = shared;
    canonical.extend(canonical_above);
    (ours, canonical)
}

/// The canonical peer: serves exactly the canonical chain by height.
struct CanonicalPeer {
    by_height: HashMap<u32, FullBlock>,
    fetches: AtomicU64,
}

impl CanonicalPeer {
    fn new(canonical: &[FullBlock]) -> Self {
        Self {
            by_height: canonical.iter().map(|b| (b.height(), b.clone())).collect(),
            fetches: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl BlockRangeSource for CanonicalPeer {
    fn peer_id(&self) -> u64 {
        0x71
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        self.fetches.fetch_add(1, Ordering::Relaxed);
        Ok((start..=end)
            .filter_map(|h| self.by_height.get(&h).cloned())
            .collect())
    }
}

async fn chaser_on_wrong_peak(
    ours: &[FullBlock],
) -> Chaser<Arc<dg_xch_stores::SqliteStore>, NativePrimitives> {
    let store = Arc::new(common::new_store().await);
    common::ancestry::seed_synthetic_parent(&store, &ours[0]).await;
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
        .follow_blocks(ours)
        .await
        .expect("our branch (with the wrong peak block) confirms");
    assert_eq!(
        peak,
        Some((ours.last().unwrap().header_hash().unwrap(), PEAK)),
        "precondition: the confirmed peak IS the wrong block X at {PEAK}"
    );
    chaser
}

// The wedge reproduction at the torn-peak boundary: every forward window from peak+1 fetches
// canonical PEAK+1, whose parent is canonical Y — unknown to a store holding X — so the follow
// orphans, and retrying can never converge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forward_follow_over_a_wrong_peak_block_wedges_with_orphan_error() {
    let (ours, canonical) = fixture_chains();
    let mut chaser = chaser_on_wrong_peak(&ours).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(CanonicalPeer::new(&canonical));

    for attempt in 0..2u32 {
        let err = chaser
            .follow_to(&peer, PEAK + 1, B_TIP)
            .await
            .expect_err("the forward window must not confirm across the unknown canonical parent");
        assert!(
            err.is_orphan(),
            "attempt {attempt}: expected the orphan wedge, got: {err}"
        );
    }
    let peak = chaser.engine().store().get_peak().await.unwrap();
    assert_eq!(
        peak,
        Some((ours.last().unwrap().header_hash().unwrap(), PEAK)),
        "plain forward retries leave the node wedged on the wrong peak block"
    );
}

// The backtrack's depth-0 probe (the peak height itself) fetches canonical Y, whose parent (our
// FORK block) is known — the fork point is found at exactly peak - 1. Y parks as an equal-weight
// orphan candidate; canonical PEAK+1 is the first block to outweigh X, and the engine reorgs
// ATOMICALLY through the candidate branch [Y, PEAK+1] — the peak leaves the wrong block and
// converges to the canonical tip.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backtrack_flips_a_wrong_block_at_the_peak_to_the_canonical_branch() {
    let (ours, canonical) = fixture_chains();
    let mut chaser = chaser_on_wrong_peak(&ours).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(CanonicalPeer::new(&canonical));

    let err = chaser
        .follow_to(&peer, PEAK + 1, B_TIP)
        .await
        .expect_err("forward window orphans first");
    assert!(err.is_orphan());

    let (peak, _deltas) = chaser
        .follow_backtrack_reporting(&peer, PEAK + 1, B_TIP)
        .await
        .expect("backtrack finds the fork at peak - 1 and converges")
        .into_result()
        .expect("window accepted");

    assert_eq!(
        peak,
        Some((canonical.last().unwrap().header_hash().unwrap(), B_TIP)),
        "the confirmed peak flipped off the wrong block and advanced to the canonical tip"
    );
    // The reorging block is canonical PEAK+1 (the first to outweigh the equal-weight sibling);
    // fork height is PEAK-1 — depth 2 per the metric's documented meaning.
    assert_eq!(
        chaser.metrics().last_reorg_depth.load(Ordering::Relaxed),
        u64::from(PEAK + 1 - FORK),
        "reorg depth = reorging block height ({}) - fork height ({FORK})",
        PEAK + 1
    );
    // And the wrong block X is no longer the main-chain occupant of its height.
    let at_peak_height = chaser
        .engine()
        .store()
        .get_block_record_by_height(PEAK)
        .await
        .unwrap()
        .expect("a main-chain record at the contested height");
    assert_eq!(
        at_peak_height.header_hash,
        canonical[(PEAK - 100) as usize].header_hash().unwrap(),
        "the canonical block Y owns the contested height after the flip"
    );
}
