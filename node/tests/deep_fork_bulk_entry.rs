// Deep-fork BULK ENTRY — the band between the short-sync backtrack floor
// (`BACKTRACK_MAX_DEPTH` = 5) and the WP-anchored long-sync band: a fork ~6-50 below our
// confirmed peak. The new-peak ladder answers it by falling through the failed backtrack to batch
// sync, whose downloads re-enter weight-only fork choice. Here the backtrack
// signals `SyncError::DeepFork` (the escalation, pinned in backtrack.rs), and the server's
// deep-fork arm re-enters through the BULK pipeline — headers-first candidates + the
// reservation-window out-of-order body download + per-block confirm (`Chaser::sync_range`, the
// exact path `Node::bulk_sync`'s `fast_sync_with_summaries` drives) — where the engine's
// single-transaction reorg flips the chain the moment the branch outweighs the peak. These pin
// that entry:
//
//   1. the mid-band escalation: a fork with a REAL shared ancestor 20 back (not a disjoint
//      chain) still signals DeepFork after a bounded backward probe, store untouched;
//   2. the bulk entry itself: candidate records seeded for the forked branch (duplicate heights
//      against the confirmed chain), bodies write through out-of-order, the branch parks as
//      orphan candidates during the in-order confirm, and ONE atomic reorg lands the flip —
//      confirmed by-height chain, peak, reorg-depth metric, and winning-branch coin state all
//      checked;
//   3. the crash contract inside the bulk entry: a store fault at the peak-flip seam (between
//      the fork rollback and the pointer flip — both inside the ONE reorg transaction) leaves
//      the store exactly at the pre-reorg state; the KILLED process's store reopens cold, and
//      the recovery follow reconstructs the whole 26-deep branch FROM THE DURABLE STORE
//      (`delta_from_store`, the store-backed fork walk, across a restart, at bulk-entry depth)
//      and lands the reorg with the wallet-facing rollback deltas attached.
//
// Fixture conventions follow backtrack.rs / long_sync_reland.rs: re-stamped real mainnet bodies
// on synthetic (height, weight, prev) links; assume_valid above every synthetic height.

mod common;

use async_trait::async_trait;
use common::fault::FaultStore;
use common::seed_record_for;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig, SyncError};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE_WEIGHT: u128 = 1_000_000;
const FORK: u32 = 130; // 20 below our peak: past the backtrack cap (5), far short of a WP band
const A_TIP: u32 = 150; // our confirmed peak (the minority branch after the network reorged)
const B_TIP: u32 = 170; // the network's heavier chain, forked at FORK

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

// Branch B gains 8/block from the fork: B(131..=155) are all LIGHTER than our A(150) peak
// (a_weight(150) = BASE+500; b_weight(155) = BASE+300+200 = BASE+500, not heavier) and must park
// as orphan candidates through the bulk confirm; B(156) = BASE+508 is the first to outweigh —
// the reorg fires there, 26 deep, through the engine's single-transaction fork choice.
fn b_weight(h: u32) -> u128 {
    a_weight(FORK) + u128::from(h - FORK) * 8
}

fn fixture_chains() -> (Vec<FullBlock>, Vec<FullBlock>) {
    let base_a = common::load_full_block(5_000_000);
    let base_b = common::load_full_block(5_000_004);
    let chain_a = build_chain(&base_a, 100, A_TIP, common::synth_hash(0xaa, 99), a_weight);
    let fork_hash = chain_a[(FORK - 100) as usize].header_hash().unwrap();
    let chain_b = build_chain(&base_b, FORK + 1, B_TIP, fork_hash, b_weight);
    (chain_a, chain_b)
}

// The peer on branch B: shared history through FORK, branch B above it.
struct ForkedPeer {
    by_height: HashMap<u32, FullBlock>,
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
        Self { by_height }
    }
}

#[async_trait]
impl BlockRangeSource for ForkedPeer {
    fn peer_id(&self) -> u64 {
        0x6d
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

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 64,
        batch: 16,
        request_timeout: Duration::from_secs(20),
        assume_valid: 10_000_000,
    }
}

async fn chaser_on_branch_a<S>(store: S, chain_a: &[FullBlock]) -> Chaser<S, NativePrimitives>
where
    S: CoinStore + BlockStore + Clone + Send + Sync + 'static,
{
    let engine = Engine::new(store, NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, cfg());
    let peak = chaser
        .follow_blocks(chain_a)
        .await
        .expect("branch A confirms");
    assert_eq!(
        peak,
        Some((chain_a.last().unwrap().header_hash().unwrap(), A_TIP)),
        "precondition: our confirmed peak is branch A's tip"
    );
    chaser
}

// Seed the headers-first candidate records for branch B — the `sync_headers` product the bulk
// path stores before any body lands (the t052/adversarial-peer seeding convention). These are
// DUPLICATE heights against the confirmed chain: two records per height in 131..=150, keyed by
// header hash — exactly what a fork entering bulk looks like.
async fn seed_branch_b_candidates<S: BlockStore>(store: &S, chain_b: &[FullBlock]) {
    let template = common::load_records()[0].clone();
    let records: Vec<_> = chain_b
        .iter()
        .map(|b| seed_record_for(&template, b))
        .collect();
    store
        .add_block_records(&records)
        .await
        .expect("seed branch B candidates");
}

// The winning-chain coin domain: block 5_000_004's real ADDITIONS (the branch-B body's own
// coins). Re-stamping applies the SAME real coin set at every synthetic height (a
// duplicate-creation shape no real chain has — each re-apply overwrites the single row), so any
// coin the OTHER body touches carries a fixture-artifact row state after the flip: 5_000_000's
// additions were re-confirmed above the fork by branch A (and 5_000_004's removals spend some
// of them), so the rollback deletes rows a from-scratch replay keeps at their below-fork
// heights. Those names are excluded — the exact per-height unwind semantics are pinned with
// unique per-height lineages in long_reorg_scale.rs. Here the domain is the winning branch's
// own coins, which must byte-equal a from-scratch confirm of the winning chain.
fn b_coin_names() -> Vec<Bytes32> {
    use dg_xch_core::traits::SizedBytes;
    use std::collections::HashSet;
    let (a_adds, _) = common::load_adds_rems(5_000_000);
    let base_names: HashSet<Bytes32> = a_adds.iter().map(|c| c.coin.name()).collect();
    let (adds, _) = common::load_adds_rems(5_000_004);
    let mut names: Vec<Bytes32> = adds
        .iter()
        .map(|c| c.coin.name())
        .filter(|n| !base_names.contains(n))
        .collect();
    names.sort_by_key(|n| n.bytes());
    names.dedup();
    names
}

async fn coin_states(
    store: &impl CoinStore,
    names: &[Bytes32],
) -> Vec<(
    Bytes32,
    Option<dg_xch_core::blockchain::coin_record::CoinRecord>,
)> {
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        out.push((*n, store.get_coin_record(n).await.unwrap()));
    }
    out
}

// Confirm the winning chain [A..=FORK] + branch B from scratch through the same pipeline and
// return the reference coin states — the replay the reorged store must equal.
async fn winning_chain_replay(
    chain_a: &[FullBlock],
    chain_b: &[FullBlock],
    names: &[Bytes32],
) -> Vec<(
    Bytes32,
    Option<dg_xch_core::blockchain::coin_record::CoinRecord>,
)> {
    let store = Arc::new(common::new_store().await);
    let engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, cfg());
    let mut chain: Vec<FullBlock> = chain_a
        .iter()
        .filter(|b| b.height() <= FORK)
        .cloned()
        .collect();
    chain.extend(chain_b.iter().cloned());
    let peak = chaser.follow_blocks(&chain).await.expect("replay confirms");
    assert_eq!(
        peak,
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "replay lands on the branch-B tip"
    );
    coin_states(store.as_ref(), names).await
}

async fn assert_on_branch_b<S: CoinStore + BlockStore>(
    store: &S,
    chain_a: &[FullBlock],
    chain_b: &[FullBlock],
    context: &str,
) {
    assert_eq!(
        store.get_peak().await.unwrap(),
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "{context}: peak is the branch-B tip"
    );
    for b in chain_b {
        let rec = store
            .get_block_record_by_height(b.height())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{context}: height {} confirmed", b.height()));
        assert_eq!(
            rec.header_hash,
            b.header_hash().unwrap(),
            "{context}: height {} must be branch B on the confirmed chain",
            b.height()
        );
    }
    let names = b_coin_names();
    assert!(!names.is_empty(), "the branch body carries real coins");
    let actual = coin_states(store, &names).await;
    let expected = winning_chain_replay(chain_a, chain_b, &names).await;
    for (a, e) in actual.iter().zip(expected.iter()) {
        assert_eq!(
            a, e,
            "{context}: winning-branch coin state must equal a from-scratch confirm"
        );
    }
}

async fn assert_still_on_branch_a<S: CoinStore + BlockStore>(
    store: &S,
    chain_a: &[FullBlock],
    context: &str,
) {
    assert_eq!(
        store.get_peak().await.unwrap(),
        Some((chain_a.last().unwrap().header_hash().unwrap(), A_TIP)),
        "{context}: peak is still branch A's tip"
    );
    for b in chain_a {
        let rec = store
            .get_block_record_by_height(b.height())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{context}: height {} confirmed", b.height()));
        assert_eq!(
            rec.header_hash,
            b.header_hash().unwrap(),
            "{context}: height {} must still be branch A",
            b.height()
        );
    }
}

// (1) The mid-band escalation with a REAL shared ancestor: the fork is 20 back — the backward
// probe reaches only peak-4, finds no known parent (every probed height is branch B), and must
// signal DeepFork after a bounded number of fetches, store untouched. backtrack.rs pins this for
// a fully disjoint chain; the mid-band shape (shared history BELOW the probe floor) is the one
// the bulk entry then resolves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn midband_fork_with_a_shared_ancestor_signals_deep_fork() {
    let (chain_a, chain_b) = fixture_chains();
    let store = Arc::new(common::new_store().await);
    let mut chaser = chaser_on_branch_a(store, &chain_a).await;
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, &chain_b));

    // The forward window orphans (the wedge), then the backtrack trips the cap.
    let err = chaser
        .follow_to(&peer, A_TIP + 1, B_TIP)
        .await
        .expect_err("forward follow across the mid-band fork must orphan");
    assert!(err.is_orphan(), "expected the orphan wedge, got: {err}");
    let err = chaser
        .follow_backtrack_reporting(&peer, A_TIP + 1, B_TIP)
        .await
        .expect_err("a 20-deep fork is past the backtrack cap");
    let SyncError::DeepFork { base, floor } = err else {
        panic!("expected DeepFork, got: {err}");
    };
    assert_eq!(base, A_TIP + 1);
    assert_eq!(floor, A_TIP - dg_xch_node::sync::BACKTRACK_MAX_DEPTH);
    assert_still_on_branch_a(chaser.engine().store().as_ref(), &chain_a, "after DeepFork").await;
}

// (2) The bulk entry lands the mid-band reorg: branch-B candidates seeded (headers-first),
// bodies through the reservation-window write-through, per-block confirm parks 131..=155 as
// orphan candidates and flips atomically at B(156) — then extends to the branch tip. The store
// walk found the true fork (depth metric), and the winning-branch coin state equals a
// from-scratch confirm.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bulk_entry_reorgs_a_midband_fork_through_the_download_confirm_pipeline() {
    let (chain_a, chain_b) = fixture_chains();
    let store = Arc::new(common::new_store().await);
    let mut chaser = chaser_on_branch_a(store.clone(), &chain_a).await;
    seed_branch_b_candidates(store.as_ref(), &chain_b).await;

    let mut pending = store.get_unassociated(256).await.unwrap();
    pending.sort_unstable();
    assert_eq!(
        pending,
        (FORK + 1..=B_TIP).collect::<Vec<_>>(),
        "the bulk ledger owes every branch-B body, duplicate heights included"
    );

    let sources: Vec<Arc<dyn BlockRangeSource>> =
        vec![Arc::new(ForkedPeer::new(&chain_a, &chain_b))];
    // Wall bound generous for debug builds: the bulk confirm runs the branch's 40 transaction
    // bodies' CLVM inline (sync_range is the per-block add_block path, no window precompute).
    let peak = tokio::time::timeout(Duration::from_secs(300), chaser.sync_range(&sources))
        .await
        .expect("bulk entry must not wedge")
        .expect("bulk entry confirms");
    assert_eq!(
        peak,
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "the bulk confirm flipped onto branch B and extended to its tip"
    );
    assert_eq!(
        chaser.metrics().last_reorg_depth.load(Ordering::Relaxed),
        26,
        "reorg depth = reorging block height (156) - fork height (130)"
    );
    assert_on_branch_b(store.as_ref(), &chain_a, &chain_b, "after the bulk entry").await;
}

// (3) The crash contract inside the bulk entry, plus the killed process's recovery. The fault
// fires on `set_peak_in` INSIDE the single reorg transaction — after `rollback_to_in` and all
// 26 branch re-applies already executed in it — so "between the rollback and the peak flip" is
// exactly the window modeled. The transaction never commits: the store must be EXACTLY the
// pre-reorg state. Then the process dies (everything dropped), the store file reopens cold, and
// a fresh chaser's forward follow re-lands the reorg by rebuilding the ENTIRE branch from the
// durable store (`delta_from_store` — pending/staged caches are empty after the restart), with
// the wallet-facing rollback delta attached to the first re-applied block.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crash_at_the_bulk_entry_peak_flip_recovers_across_a_restart() {
    let (chain_a, chain_b) = fixture_chains();
    let path = common::unique_db_path();
    let inner = SqliteStore::open(&path).await.expect("open");
    let (fault, _fail_apply, fail_set_peak) = FaultStore::new(inner);
    let store = Arc::new(fault);
    let mut chaser = chaser_on_branch_a(store.clone(), &chain_a).await;
    seed_branch_b_candidates(store.as_ref(), &chain_b).await;

    // The branch-B coin domain BEFORE the bulk entry: the crashed flip must leave every one of
    // these states untouched (snapshot-compare, not absence — 5_000_004's removals legitimately
    // name coins the re-stamped 5_000_000 chain created below the fork).
    let b_names = b_coin_names();
    let before = coin_states(store.as_ref(), &b_names).await;

    // Arm the crash: the first set_peak of the bulk entry IS the reorg's pointer flip (orphan
    // parks never move the peak).
    fail_set_peak.store(true, Ordering::Relaxed);
    let sources: Vec<Arc<dyn BlockRangeSource>> =
        vec![Arc::new(ForkedPeer::new(&chain_a, &chain_b))];
    let err = tokio::time::timeout(Duration::from_secs(300), chaser.sync_range(&sources))
        .await
        .expect("the crashed bulk entry must not wedge")
        .expect_err("the injected peak-flip fault surfaces");
    assert!(
        err.to_string().contains("injected set_peak"),
        "the failure is the injected flip fault, got: {err}"
    );
    assert_still_on_branch_a(
        store.as_ref(),
        &chain_a,
        "after the crashed flip (atomicity: rollback + re-applies never committed)",
    )
    .await;
    // The failed transaction's coin mutations never landed either: every branch-domain coin
    // state is byte-identical to the pre-entry snapshot.
    let after = coin_states(store.as_ref(), &b_names).await;
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(
            b, a,
            "the crashed flip must leave every branch-domain coin state untouched"
        );
    }

    // The kill: drop the chaser and the faulting store wrapper; reopen the file cold.
    drop(chaser);
    drop(store);
    let reopened = Arc::new(SqliteStore::open(&path).await.expect("reopen after kill"));
    assert_still_on_branch_a(reopened.as_ref(), &chain_a, "after the cold reopen").await;

    // The restarted node: fresh engine (empty pending/staged caches), warmed from the store,
    // recovers through the ordinary forward follow — branch B's records+bodies landed durably
    // before the crash, so the 26-deep branch is rebuilt from the store and the reorg lands.
    let engine = Engine::new(reopened.clone(), NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, cfg());
    chaser.warm_engine_cache().await.expect("warm");
    let peer: Arc<dyn BlockRangeSource> = Arc::new(ForkedPeer::new(&chain_a, &chain_b));
    let (peak, deltas) = chaser
        .follow_tip_step_reporting(&peer, A_TIP + 1, B_TIP)
        .await
        .expect("the recovery follow re-lands the reorg from the durable store");
    assert_eq!(
        peak,
        Some((chain_b.last().unwrap().header_hash().unwrap(), B_TIP)),
        "the restarted node converged onto branch B"
    );
    // The wallet feed: the rollback delta is attached to the FIRST re-applied branch block, and
    // the reported deltas cover the whole branch fork+1..=tip.
    let first = deltas
        .iter()
        .find(|d| d.reorg.is_some())
        .expect("the landed reorg surfaces a wallet-facing rollback delta");
    assert_eq!(first.delta.height, FORK + 1, "attached to fork+1");
    let wallet = first.reorg.as_ref().unwrap();
    assert_eq!(wallet.fork_height, FORK, "the true fork height");
    assert!(
        !wallet.rolled_back.is_empty(),
        "the abandoned span's post-rollback coin states are reported"
    );
    let reported: Vec<u32> = deltas.iter().map(|d| d.delta.height).collect();
    assert_eq!(
        reported,
        (FORK + 1..=B_TIP).collect::<Vec<_>>(),
        "every winning-branch block above the fork is reported in height order"
    );
    assert_on_branch_b(reopened.as_ref(), &chain_a, &chain_b, "after the recovery").await;
}
