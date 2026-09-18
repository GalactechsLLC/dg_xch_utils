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

// The live wedge (mainnet 9,147,575): the store's tip sits on an orphaned branch after a reorg at/below
// it; every forward follow window from peak+1 fetches blocks whose FIRST parent is unknown, the engine
// raises NodeError::Orphan ("unknown parent ..."), and the driver retries the identical window forever.
// The short-sync backtrack resolves this: fetch blocks BACKWARD one at a time from the claimed
// tip until one connects to our chain (cap: fork no deeper than 5 below our peak), then add the
// collected chain lowest-first; a deeper fork falls through to long sync. These tests pin that
// driver behavior on our Chaser.

const BASE_WEIGHT: u128 = 1_000_000;
const FORK: u32 = 103; // fork point: last common block of chains A and B
const A_TIP: u32 = 105; // our confirmed peak (on the now-orphaned branch A)
const B_TIP: u32 = 110; // the peer's claimed peak (on the heavier branch B)

// Re-stamp a real mainnet body onto a synthetic (height, weight, prev) chain link. The foliage's
// prev_block_hash is the chain link AND the header-hash domain (header_hash = hash(foliage)), so
// distinct branches must re-stamp DIFFERENT base bodies to get distinct hashes at equal heights.
fn link_block(base: &FullBlock, h: u32, weight: u128, prev: Bytes32) -> FullBlock {
    let mut b = base.clone();
    b.reward_chain_block.height = h;
    b.reward_chain_block.weight = weight;
    b.foliage.prev_block_hash = prev;
    b
}

// A linked chain of re-stamped bodies: heights `start..=end`, first parent `prev0`, per-height weight
// `weight_at(h)` (strictly increasing along the chain; make it heavier than the competing branch to
// force the reorg).
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

// Branch B: slower per-block weight gain than A (8 < 10), so B(104) and B(105) are LIGHTER than our
// A(105) peak and must park as orphan candidates; B(106) is the first block to outweigh the peak
// (a_weight(105) = BASE+50 < BASE+30+24 = b_weight(106)) — the reorg fires there, through the engine's
// pending-branch fork choice, exactly like an in-order arrival would have.
fn b_weight(h: u32) -> u128 {
    a_weight(FORK) + u128::from(h - FORK) * 8
}

// The peer: it sits on branch B and serves exactly its own chain by height — shared history through
// FORK (branch A's blocks), branch B above it. Counts fetches so the tests can assert the backtrack
// is bounded (no infinite retry).
struct ForkedPeer {
    by_height: HashMap<u32, FullBlock>,
    fetches: AtomicU64,
}

impl ForkedPeer {
    fn new(chain_a: &[FullBlock], chain_b: &[FullBlock]) -> Self {
        let mut by_height = HashMap::new();
        for b in chain_a.iter().filter(|b| b.height() <= FORK) {
            by_height.insert(b.height(), b.clone());
        }
        for b in chain_b {
            by_height.insert(b.height(), b.clone());
        }
        Self {
            by_height,
            fetches: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl BlockRangeSource for ForkedPeer {
    fn peer_id(&self) -> u64 {
        0xb7
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

// A chaser confirmed onto branch A (heights 100..=A_TIP) — the pre-wedge state. assume_valid is set
// above every synthetic height so the expensive body checks (BLS aggregate + block conditions) are
// bypassed exactly like a below-milestone mainnet block; coin deltas and the header/pospace path
// still run for real.
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
        "precondition: our peak is branch A's tip"
    );
    chaser
}

fn fixture_chains() -> (Vec<FullBlock>, Vec<FullBlock>) {
    let base_a = common::load_full_block(5_000_000);
    let base_b = common::load_full_block(5_000_004);
    // Branch A: 100..=105 from an unknown-parent bootstrap entry (empty store accepts a checkpoint base).
    let chain_a = build_chain(&base_a, 100, A_TIP, common::synth_hash(0xaa, 99), a_weight);
    // Branch B forks off A at FORK: B(FORK+1..=B_TIP), first parent = A(FORK).
    let fork_hash = chain_a[(FORK - 100) as usize].header_hash().unwrap();
    let chain_b = build_chain(&base_b, FORK + 1, B_TIP, fork_hash, b_weight);
    (chain_a, chain_b)
}

// RED — the live-bug reproduction. Mainnet reorged at/below our tip: the peer serves branch B, our
// peak is branch A's tip. The forward follow window (peak+1..) must fail with the ORPHAN error (the
// exact wedge log line), and retrying the same window — all the pre-backtrack driver could do — must
// fail identically: without fetching backward the driver can never converge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forward_follow_from_an_orphaned_tip_wedges_with_orphan_error() {
    let (chain_a, chain_b) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, &chain_b));

    for attempt in 0..2u32 {
        let err = chaser
            .follow_to(&peer, A_TIP + 1, B_TIP)
            .await
            .expect_err("the forward window must not confirm across an unknown parent");
        assert!(
            err.is_orphan(),
            "attempt {attempt}: expected the orphan wedge, got: {err}"
        );
    }
    // The peak never moved — the retry loop is a genuine wedge.
    let peak = chaser.engine().store().get_peak().await.unwrap();
    assert_eq!(
        peak,
        Some((chain_a.last().unwrap().header_hash().unwrap(), A_TIP)),
        "plain forward retries leave the node wedged on branch A"
    );
}

// The short-sync backtrack: on the orphan wedge,
// fetch single blocks backward from below the failed window until one's parent is known (here
// B(FORK+1), whose parent A(FORK) we have), then submit the collected chain lowest-first plus the
// forward window; the engine's existing fork choice reorgs to branch B. The wedge resolves with no
// operator action and the reorg-depth metric is set.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backtrack_converges_to_the_fork_branch_tip() {
    let (chain_a, chain_b) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, &chain_b));

    let err = chaser
        .follow_to(&peer, A_TIP + 1, B_TIP)
        .await
        .expect_err("forward window orphans first");
    assert!(err.is_orphan());

    let (peak, _deltas) = chaser
        .follow_backtrack_reporting(&peer, A_TIP + 1, B_TIP)
        .await
        .expect("backtrack finds the fork point and converges")
        .into_result()
        .expect("window accepted");

    assert_eq!(
        peak,
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "the confirmed peak advanced to branch B's tip"
    );
    // The metric's documented meaning: reorging block height minus fork height, at the block that
    // flipped the branches — B(106) over fork point 103.
    assert_eq!(
        chaser.metrics().last_reorg_depth.load(Ordering::Relaxed),
        3,
        "reorg depth = reorging block height (106) - fork height (103)"
    );
}

// The bounded-depth fallback: the backtrack is capped at 5 below the peak; a fork deeper than
// that falls through to long sync. A peer whose branch shares NO ancestor within the cap must yield
// the distinct DeepFork signal after a bounded number of single-block fetches — never an infinite retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fork_deeper_than_the_backtrack_cap_signals_long_sync() {
    let (chain_a, _) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;

    // A peer on a FULLY disjoint chain: same heights, different bodies (base 5_000_004), no shared
    // ancestor anywhere — every backtracked parent is unknown, so the cap must trip.
    let base_b = common::load_full_block(5_000_004);
    let disjoint = build_chain(&base_b, 100, B_TIP, common::synth_hash(0xbb, 99), |h| {
        a_weight(h.min(A_TIP)) + u128::from(h) // heavier, but never connecting
    });
    let peer = Arc::new(ForkedPeer::new(&[], &disjoint));
    let src: Arc<dyn BlockRangeSource> = peer.clone();

    let before = peer.fetches.load(Ordering::Relaxed);
    let err = chaser
        .follow_backtrack_reporting(&src, A_TIP + 1, B_TIP)
        .await
        .expect_err("no fork point within the cap");
    assert!(
        matches!(err, SyncError::DeepFork { .. }),
        "expected the distinct needs-long-sync signal, got: {err}"
    );
    let single_fetches = peer.fetches.load(Ordering::Relaxed) - before;
    assert!(
        single_fetches <= u64::from(dg_xch_node::sync::BACKTRACK_MAX_DEPTH),
        "backtrack is bounded by the cap (made {single_fetches} fetches)"
    );

    // The store is untouched: still wedged on A, awaiting the caller's long-sync path.
    let peak = chaser.engine().store().get_peak().await.unwrap();
    assert_eq!(
        peak,
        Some((chain_a.last().unwrap().header_hash().unwrap(), A_TIP))
    );
}

// ── tip-follow ladder: forward-extend first, backtrack only on the orphan ─────────────────────────
//
// The near-tip deviation: the tip_follower drove EVERY step through `follow_backtrack_reporting`,
// whose depth-0 probe is `from-1` (our own peak). So a plain direct child of the peak still triggered
// a backward peak-refetch + a peer round-trip per block — the overhead that kept a live follower at
// lag 3-6 instead of 0-1. `follow_tip_step_reporting` restores the ladder: forward first.

// A peer that serves a chain by height and records the LOWEST height any fetch reached, so a test can
// distinguish a forward-only extend (lowest == peak+1) from a backward backtrack probe (lowest <= peak).
struct RecordingPeer {
    by_height: HashMap<u32, FullBlock>,
    lowest: AtomicU64,
}

impl RecordingPeer {
    fn new(chain: &[FullBlock]) -> Self {
        Self {
            by_height: chain.iter().map(|b| (b.height(), b.clone())).collect(),
            lowest: AtomicU64::new(u64::MAX),
        }
    }
    fn lowest_fetched(&self) -> u64 {
        self.lowest.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl BlockRangeSource for RecordingPeer {
    fn peer_id(&self) -> u64 {
        0xd1
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        self.lowest.fetch_min(u64::from(start), Ordering::Relaxed);
        Ok((start..=end)
            .filter_map(|h| self.by_height.get(&h).cloned())
            .collect())
    }
}

// A linear branch A extended two blocks past our peak (direct children, no fork).
fn linear_chain_past_tip() -> (Vec<FullBlock>, Vec<FullBlock>) {
    let base_a = common::load_full_block(5_000_000);
    let chain_a = build_chain(&base_a, 100, A_TIP, common::synth_hash(0xaa, 99), a_weight);
    let child = build_chain(
        &base_a,
        A_TIP + 1,
        A_TIP + 2,
        chain_a.last().unwrap().header_hash().unwrap(),
        a_weight,
    );
    (chain_a, child)
}

// The backtrack recovery arm, driven for a DIRECT CHILD, still probes backward to the peak
// (`from-1`) before confirming — the per-block overhead the forward-first ladder avoids.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn backtrack_arm_refetches_the_peak_even_for_a_direct_child() {
    let (chain_a, child) = linear_chain_past_tip();
    let mut linear = chain_a.clone();
    linear.extend(child.iter().cloned());
    let peer = Arc::new(RecordingPeer::new(&linear));
    let mut chaser = chaser_on_branch_a(&chain_a).await;

    let source: Arc<dyn BlockRangeSource> = peer.clone();
    let (peak, _deltas) = chaser
        .follow_backtrack_reporting(&source, A_TIP + 1, A_TIP + 1)
        .await
        .expect("backtrack confirms the child too")
        .into_result()
        .expect("window accepted");
    assert_eq!(peak, Some((child[0].header_hash().unwrap(), A_TIP + 1)));
    // The deviation: the arm reached BELOW peak+1 (it probed the peak at from-1).
    assert!(
        peer.lowest_fetched() <= u64::from(A_TIP),
        "the backtrack arm fetches backward to the peak even for a direct child"
    );
}

// GREEN: the ladder extends a direct child with a single FORWARD fetch of [peak+1, peak+1] — no
// backward probe — so a live follower confirms tip in one round-trip and pins lag at 0-1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tip_step_extends_a_direct_child_forward_only() {
    let (chain_a, child) = linear_chain_past_tip();
    let mut linear = chain_a.clone();
    linear.extend(child.iter().cloned());
    let peer = Arc::new(RecordingPeer::new(&linear));
    let mut chaser = chaser_on_branch_a(&chain_a).await;

    let source: Arc<dyn BlockRangeSource> = peer.clone();
    let (peak, _deltas) = chaser
        .follow_tip_step_reporting(&source, A_TIP + 1, A_TIP + 1)
        .await
        .expect("the direct child extends via the forward arm")
        .into_result()
        .expect("window accepted");
    assert_eq!(
        peak,
        Some((child[0].header_hash().unwrap(), A_TIP + 1)),
        "the ladder advanced the peak to the direct child"
    );
    assert_eq!(
        peer.lowest_fetched(),
        u64::from(A_TIP + 1),
        "forward-only: the ladder must not fetch below peak+1 for a direct child"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tip_step_falls_back_to_backtrack_on_a_real_reorg() {
    let (chain_a, chain_b) = fixture_chains();
    let mut chaser = chaser_on_branch_a(&chain_a).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, &chain_b));

    let (peak, _deltas) = chaser
        .follow_tip_step_reporting(&peer, A_TIP + 1, B_TIP)
        .await
        .expect("the ladder recovers the reorg via the backtrack arm")
        .into_result()
        .expect("window accepted");
    assert_eq!(
        peak,
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "a genuine reorg still converges to branch B's tip through the ladder"
    );
}
