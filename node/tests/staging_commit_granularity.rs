mod common;

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE_WEIGHT: u128 = 1_000_000;
const START: u32 = 100;
const N: u32 = 8;

// A linked chain of re-stamped real mainnet bodies (the torn_peak.rs builder shape): full body
// validation runs real under follow; assume_valid covers the header signatures. Fixture note for
// read accounting: each block keeps the ORIGINAL mainnet foliage-transaction-block prev-tx hash
// (foreign to this synthetic chain), so the reward-claim ancestry walk misses the record cache
// and falls back to ONE store read per transaction block — a restamp artifact, not the staging
// seam (a live window's prev-tx ancestor is a cache hit); the read pin budgets for it.
fn build_chain(base: &FullBlock, start: u32, end: u32, prev0: Bytes32) -> Vec<FullBlock> {
    let mut prev = prev0;
    let mut out = Vec::new();
    for h in start..=end {
        let mut b = base.clone();
        b.reward_chain_block.height = h;
        b.reward_chain_block.weight = BASE_WEIGHT + u128::from(h) * 10;
        b.foliage.prev_block_hash = prev;
        prev = b.header_hash().expect("header hash");
        out.push(b);
    }
    out
}

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 32,
        batch: 32,
        request_timeout: Duration::from_secs(20),
        assume_valid: 10_000_000,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn transaction_limit_is_independent_of_validation_window() {
    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, START, START + N - 1, common::synth_hash(0xaa, 99));
    for coalesce in [false, true] {
        let store = Arc::new(common::new_store().await);
        let telemetry = store.telemetry().unwrap();
        let mut engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
        engine.set_coalesce_coin_writes(coalesce);
        let mut chaser = Chaser::new(engine, cfg());
        chaser.set_confirm_transaction_blocks(Some(3));
        let peak = chaser.follow_blocks(&chain).await.unwrap();
        assert_eq!(
            peak,
            Some((chain.last().unwrap().header_hash().unwrap(), START + N - 1))
        );
        assert_eq!(telemetry.commit_catch_up.count.load(Ordering::Relaxed), 3);
        for block in &chain {
            assert!(
                store
                    .get_block(&block.header_hash().unwrap())
                    .await
                    .unwrap()
                    .is_some()
            );
        }
    }
}

// CATCH-UP band: following one N-block window costs exactly TWO writer commits — one staging
// transaction for the whole window's archive rows, one confirm transaction for coins + peak
// (t160 already pins the confirm half). N per-block staging commits is the defect: N sequential
// fsync round-trips of dead time per window on the single sqlite writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn catch_up_window_stages_in_one_commit() {
    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, START, START + N - 1, common::synth_hash(0xaa, 99));

    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    // Catch-up band is the store default (near_tip = false); stated explicitly for the contrast
    // with the near-tip test below.
    store.set_near_tip(false);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());

    let before = telemetry.commit_catch_up.count.load(Ordering::Relaxed);
    let peak = chaser.follow_blocks(&chain).await.expect("window confirms");
    assert_eq!(
        peak,
        Some((chain.last().unwrap().header_hash().unwrap(), START + N - 1)),
        "the window confirmed to its tip"
    );
    let commits = telemetry.commit_catch_up.count.load(Ordering::Relaxed) - before;
    assert_eq!(
        commits, 1,
        "catch-up window persistence must be ONE writer transaction for the WHOLE window — the \
         staging archive rows carried into the confirm transaction (archive + coins + peak, one \
         fsync); {N} blocks paying {commits} writer round-trips leaves a separate staging commit \
         serialized ahead of the confirm on the single writer connection"
    );
}

// Read amplification of the staging loop — the residue AFTER the commit batching. Live windows
// showed vdf+body+confirm ≈ 0.78 s against a ~2.7 s wall: the staging loop's PER-BLOCK awaited
// store reads (prepare_delta's AlreadyHave probe + headers-first candidate probe + the prev-tx
// context by-height probe) serialize one point-read round-trip after another on the sqlite read
// path. Counted at the store's own read-path counter (StoreTelemetry::record_reads — every
// block-record point read, multi-get elements included), so the pin measures real store
// round-trips, not code-path guesses.
//
// Per-block reads: a 32-block catch-up window without the preload pays ~2 record reads per
// block (the AlreadyHave probe + the candidate probe, both against rows that do not exist).
// The window preload collapses
// them to ONE batched candidate fetch (N point-gets on one acquired read connection) + one peak
// read, and the prev-tx context walks the record cache instead of re-reading the store — so a
// window must cost at most N + a small constant record reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn window_staging_read_amplification_is_bounded() {
    const W: u32 = 32;
    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, START, START + W - 1, common::synth_hash(0xaa, 99));

    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    store.set_near_tip(false);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());

    let reads_before = telemetry.record_reads.load(Ordering::Relaxed);
    let peak = chaser.follow_blocks(&chain).await.expect("window confirms");
    assert_eq!(
        peak,
        Some((chain.last().unwrap().header_hash().unwrap(), START + W - 1)),
        "the window confirmed to its tip"
    );
    let reads = telemetry.record_reads.load(Ordering::Relaxed) - reads_before;
    let stage_micros = chaser.metrics().window_stage_micros.load(Ordering::Relaxed);
    // The measurement this test exists to keep visible (µs is builder-CPU-shaped; the read COUNT
    // is the live-transferable number — each count is one awaited store round-trip live).
    eprintln!("window of {W}: {reads} record reads, stage loop {stage_micros} us");
    // Budget: W for the batched candidate fetch (counted per point-get, one connection), W for
    // this fixture's reward-claim walk misses (see build_chain — an artifact, a live window's
    // walk is a cache hit), + a small constant. The seam under test is the DELTA above that:
    // per-block AlreadyHave + candidate + prev-tx probes, each one awaited store round-trip live.
    assert!(
        reads <= 2 * u64::from(W) + 4,
        "staging a {W}-block window must cost at most one batched candidate fetch + the fixture's \
         walk-miss budget, got {reads} record reads — the per-block AlreadyHave/candidate/prev-tx \
         probes are serialized store round-trips (the live catch-up residue)"
    );
}

// NEAR-TIP band: staging keeps its per-block commit (durability per block is CORRECT at tip — the
// liveness clock and the active WAL checkpointer both key on it, exactly as the confirm side's
// per-block near-tip transactions do). One N-block window near tip pays N staging commits + N
// per-block confirm commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn near_tip_window_still_stages_per_block() {
    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, START, START + N - 1, common::synth_hash(0xaa, 99));

    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    store.set_near_tip(true);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());

    let before = telemetry.commit_near_tip.count.load(Ordering::Relaxed);
    let peak = chaser.follow_blocks(&chain).await.expect("window confirms");
    assert_eq!(
        peak,
        Some((chain.last().unwrap().header_hash().unwrap(), START + N - 1)),
        "the window confirmed to its tip"
    );
    let commits = telemetry.commit_near_tip.count.load(Ordering::Relaxed) - before;
    assert_eq!(
        commits,
        u64::from(N) * 2,
        "near-tip: per-block staging commit + per-block confirm commit ({N} blocks)"
    );
}
