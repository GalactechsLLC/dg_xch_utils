// Restart / kill-point resume invariants (test class 6, reconstructed design). The resume-repair
// wall (edf019a: "Restarts never repaired it") and the WAL/commit-granularity work (872660e
// per-block commit, a0436f5 phase-aware checkpointer, b0fd91e autocheckpoint failsafe) are all
// about one contract nothing owed as a test: A KILL AT ANY POINT MUST LEAVE A RESUMABLE STORE.
// Concretely: an uncommitted batch vanishes wholly (never half-lands), committed state survives a
// reopen byte-identical, and the pipeline's resume point after a mid-range kill is EXACTLY the
// missing work — no gap (heights lost forever), no dupes (work re-done).
//
// The kill is modeled at the store seam: dropping the store handle (and any open batch) without
// commit is the crash-consistency contract SQLite's WAL gives a killed process; the reopen path
// is the server's restart path (SqliteStore::open on the same file + Chaser::warm_engine_cache).

mod common;

use common::{MemSource, MisbehavingSource, Misbehavior, candidate_record, synth_hash};
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::consensus::difficulty_adjustment::{
    consensus_walk_window, get_next_sub_slot_iters_and_difficulty,
};
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig, SyncError};
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::{BlockStore, SqliteStore};
use std::sync::Arc;
use std::time::Duration;

const BASE: u32 = 6_000_000;

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 32,
        batch: 8,
        request_timeout: Duration::from_millis(300),
        assume_valid: 5_000_001,
    }
}

// Kill-point class A: a batch dropped before commit vanishes WHOLLY on reopen — no half-landed
// bodies; the same batch committed survives the reopen. The write-through worker's unit of
// durability is the reservation batch (one begin/append_many/commit per range), so this is
// exactly the crash window a killed download leaves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uncommitted_batch_vanishes_wholly_and_committed_batch_survives_reopen() {
    let base_block = common::load_full_block(5_000_000);
    let template = common::load_records()[0].clone();
    let path = common::unique_db_path();
    let blocks: Vec<_> = (BASE..BASE + 8)
        .map(|h| common::restamp_block(&base_block, h))
        .collect();
    let hashes: Vec<_> = blocks
        .iter()
        .map(|b| b.header_hash().expect("hash"))
        .collect();

    // Uncommitted: candidates seeded (their own committed write — the store's FK makes bodies
    // impossible without records, itself a crash-consistency property), then the body batch is
    // begun + appended and KILLED (dropped without commit).
    {
        let store = SqliteStore::open(&path).await.expect("open");
        let records: Vec<_> = (BASE..BASE + 8)
            .map(|h| candidate_record(&template, &base_block, h))
            .collect();
        store.add_block_records(&records).await.expect("seed");
        let batch = {
            let mut batch = store.begin().await.expect("begin");
            store
                .append_many(&mut batch, &blocks)
                .await
                .expect("append");
            batch
        };
        drop(batch); // the kill: transaction never committed
        drop(store);
    }
    {
        let store = SqliteStore::open(&path).await.expect("reopen after kill");
        for (i, hh) in hashes.iter().enumerate() {
            assert!(
                store.get_block(hh).await.expect("get_block").is_none(),
                "body {i} half-landed from an uncommitted batch"
            );
        }
        let mut pending = store.get_unassociated(16).await.expect("unassociated");
        pending.sort_unstable();
        let expected: Vec<u32> = (BASE..BASE + 8).collect();
        assert_eq!(
            pending, expected,
            "after the kill the resume ledger names the whole un-landed range"
        );
        drop(store);
    }

    // Committed: the identical batch with commit survives the reopen byte-identical.
    {
        let store = SqliteStore::open(&path).await.expect("reopen");
        let mut batch = store.begin().await.expect("begin");
        store
            .append_many(&mut batch, &blocks)
            .await
            .expect("append");
        store.commit(batch).await.expect("commit");
        drop(store);
    }
    let store = SqliteStore::open(&path).await.expect("final reopen");
    for (i, hh) in hashes.iter().enumerate() {
        let got = store
            .get_block(hh)
            .await
            .expect("get_block")
            .unwrap_or_else(|| panic!("committed body {i} lost across reopen"));
        assert_eq!(
            got.to_bytes(ChiaProtocolVersion::default()).expect("bytes"),
            blocks[i]
                .to_bytes(ChiaProtocolVersion::default())
                .expect("bytes"),
            "committed body {i} survives byte-identical"
        );
    }
}

// Kill-point class B: a confirmed peak survives the restart — peak pointer, record, and body all
// intact after reopen; the rewarmed engine accepts the resumed follow (re-seeing the confirmed
// block is AlreadyHave, never an error or a re-confirm).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirmed_peak_survives_reopen_and_the_rewarmed_engine_resumes() {
    let block = common::load_full_block(5_000_000);
    let hh = block.header_hash().expect("hash");
    let path = common::unique_db_path();

    // Session 1: confirm the block, then KILL (drop everything).
    {
        let store = SqliteStore::open(&path).await.expect("open");
        let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
        let mut chaser = Chaser::new(engine, cfg());
        let peak = chaser
            .follow_blocks(std::slice::from_ref(&block))
            .await
            .expect("confirm");
        assert_eq!(peak, Some((hh, 5_000_000)), "session 1 confirmed the peak");
    }

    // Session 2: the restart. Everything the confirm wrote must be there, and the rewarmed
    // engine must resume the follow from it.
    let store = SqliteStore::open(&path).await.expect("reopen");
    assert_eq!(
        store.get_peak().await.expect("get_peak"),
        Some((hh, 5_000_000)),
        "the confirmed peak pointer survives the restart"
    );
    let record = store
        .get_block_record(&hh)
        .await
        .expect("get_block_record")
        .expect("the confirmed record survives the restart");
    assert_eq!(record.height, 5_000_000);
    let body = store
        .get_block(&hh)
        .await
        .expect("get_block")
        .expect("the confirmed body survives the restart");
    assert_eq!(
        body.to_bytes(ChiaProtocolVersion::default())
            .expect("bytes"),
        block
            .to_bytes(ChiaProtocolVersion::default())
            .expect("bytes"),
        "the confirmed body survives byte-identical"
    );

    let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, cfg());
    let warmed = chaser.warm_engine_cache().await.expect("warm cache");
    assert!(warmed >= 1, "the rewarmed cache loads the confirmed record");
    let peak = chaser
        .follow_blocks(std::slice::from_ref(&block))
        .await
        .expect("re-seeing the confirmed block after restart must not error");
    assert_eq!(
        peak,
        Some((hh, 5_000_000)),
        "the resumed follow holds the same peak (AlreadyHave, no re-confirm)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mid_range_kill_resumes_with_exactly_the_missing_heights() {
    let base_block = common::load_full_block(5_000_000);
    let template = common::load_records()[0].clone();
    let path = common::unique_db_path();
    let n = 32u32;
    let stale_tip = BASE + 15; // serves [BASE, BASE+15], rejects above

    // Session 1: seed the full candidate range, download only what the stale peer can serve,
    // then die on Exhausted (the mid-range kill).
    {
        let store = SqliteStore::open(&path).await.expect("open");
        let records: Vec<_> = (BASE..BASE + n)
            .map(|h| candidate_record(&template, &base_block, h))
            .collect();
        store.add_block_records(&records).await.expect("seed");
        let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
        let mut chaser = Chaser::new(engine, cfg());
        let sources: Vec<Arc<dyn BlockRangeSource>> = vec![Arc::new(MisbehavingSource::new(
            9,
            Misbehavior::StalePeak { tip: stale_tip },
            base_block.clone(),
        ))];
        let err = chaser
            .sync_bodies(&sources)
            .await
            .expect_err("the stale-only sync must fail, not silently succeed");
        assert!(
            matches!(err, SyncError::Exhausted(_)),
            "the mid-range kill surfaces Exhausted, got {err:?}"
        );
    }

    // Session 2: the restart. The resume point is exactly the missing upper half.
    let store = SqliteStore::open(&path).await.expect("reopen");
    let mut missing = store
        .get_unassociated(usize::try_from(n).expect("n fits"))
        .await
        .expect("unassociated");
    missing.sort_unstable();
    let expected: Vec<u32> = (stale_tip + 1..BASE + n).collect();
    assert_eq!(
        missing, expected,
        "the resume ledger names exactly the un-downloaded heights — no gap, no dupe"
    );

    // The resumed sync downloads exactly the missing count and drains.
    let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, cfg());
    let sources: Vec<Arc<dyn BlockRangeSource>> = vec![Arc::new(MemSource {
        id: 1,
        base: base_block.clone(),
    })];
    chaser.sync_bodies(&sources).await.expect("resumed sync");
    let leftover = chaser
        .engine()
        .store()
        .get_unassociated(10)
        .await
        .expect("unassociated");
    assert!(leftover.is_empty(), "the resumed range drained");
    let downloaded = chaser
        .metrics()
        .blocks_downloaded
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        downloaded,
        u64::from(n - (stale_tip + 1 - BASE)),
        "the resume re-downloads EXACTLY the missing bodies — no dupe work"
    );
}

// ---------------------------------------------------------------------------------------------
// Kill-point classes D-F: the restart-resume REPAIR holes (the observed fleet stall). A restart
// leaves the walk cache colder than the store, and the store itself can carry an anchored span or
// a mid-span record hole; the stage-path consensus walks must resolve from the store (cache
// miss → store read) instead of livelocking the MissingRecord recovery on the identical window.
// ---------------------------------------------------------------------------------------------

const WEIGHT_BASE: u128 = 1_000_000; // common::RESTAMP_BASE_WEIGHT — records chain with restamped blocks

fn ses() -> SubEpochSummary {
    SubEpochSummary {
        prev_subepoch_summary_hash: Bytes32::default(),
        reward_chain_hash: Bytes32::default(),
        num_blocks_overflow: 0,
        new_difficulty: None,
        new_sub_slot_iters: None,
    }
}

// A linked synthetic walk record. `hash` supplies the header-hash convention (the restamp tests
// use common::synth_hash so real restamped bodies chain onto the records); `tx` = carries a
// timestamp (a transaction block to the walks); `slot` = ends a sub-slot (warm-ancestry evidence);
// `with_ses` = includes a sub-epoch summary (the previous-epoch anchor the retarget scan exits at).
#[allow(clippy::fn_params_excessive_bools)]
fn walk_record(
    hash: fn(u32) -> Bytes32,
    h: u32,
    tx: bool,
    slot: bool,
    with_ses: bool,
) -> BlockRecord {
    BlockRecord {
        header_hash: hash(h),
        prev_hash: hash(h.wrapping_sub(1)),
        height: h,
        weight: WEIGHT_BASE + u128::from(h),
        total_iters: 10_000_000 * u128::from(h),
        signage_point_index: 0,
        challenge_vdf_output: ClassgroupElement::get_default_element(),
        infused_challenge_vdf_output: None,
        reward_infusion_new_challenge: Bytes32::default(),
        challenge_block_info_hash: Bytes32::default(),
        sub_slot_iters: MAINNET.sub_slot_iters_starting,
        pool_puzzle_hash: Bytes32::default(),
        farmer_puzzle_hash: Bytes32::default(),
        required_iters: 1,
        deficit: 0,
        overflow: false,
        prev_transaction_block_height: h.wrapping_sub(1),
        timestamp: tx.then(|| 1_000 + u64::from(h)),
        prev_transaction_block_hash: None,
        fees: None,
        reward_claims_incorporated: None,
        finished_challenge_slot_hashes: slot.then(|| vec![Bytes32::default()]),
        finished_infused_challenge_slot_hashes: None,
        finished_reward_slot_hashes: None,
        sub_epoch_summary_included: with_ses.then(ses),
    }
}

fn plain_hash(n: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    Bytes32::from(b)
}

// Kill-point class D: the restart warm must cover the DEEPEST retarget walk. The epoch-turn walk
// from a first-sub-epoch anchor reads up to 5,503 records (and its two-transaction-block scan can
// pierce further below the previous epoch surpass across a non-transaction run) — a flat 5,120
// warm window re-warms LESS than the walk reads, so a restart at the worst alignment walls on
// "block record not found" with every needed record sitting in the store. The warm must load the
// constants-derived walk window, and the walk over the warmed cache must compute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_warm_covers_the_deepest_retarget_walk() {
    const B: u32 = 2_000 * 4_608; // epoch boundary
    // The deepest first-sub-epoch trigger offset the walk itself admits: the
    // `height_in_next_epoch` guard caps triggers at offset 362 (o + 2*MAX_SUB_SLOT_BLOCKS +
    // MIN_BLOCKS_PER_CHALLENGE_BLOCK + 5 < 5*MAX_SUB_SLOT_BLOCKS past the surpass).
    const ANCHOR: u32 = B + 362;
    const BOTTOM: u32 = B - 5_800;
    // Non-transaction run directly below the previous-epoch SES anchor: the two-transaction-block
    // scan crosses it, reading ~95 records below the flat 5,120 window's floor.
    const RUN_LOW: u32 = B - 4_830;
    const RUN_HIGH: u32 = B - 4_607;
    const SES_AT: u32 = B - 4_606;

    let path = common::unique_db_path();
    let store = Arc::new(SqliteStore::open(&path).await.expect("open"));
    let chain: Vec<BlockRecord> = (BOTTOM..=ANCHOR)
        .map(|h| {
            let tx = !(RUN_LOW..=RUN_HIGH).contains(&h);
            walk_record(plain_hash, h, tx, false, h == SES_AT)
        })
        .collect();
    store.add_block_records(&chain).await.expect("seed chain");
    store.set_peak(&plain_hash(ANCHOR)).await.expect("set peak");

    let mut engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
    let warmed = engine.warm_cache_from_store().await.expect("warm");
    let window = consensus_walk_window(&MAINNET);
    assert_eq!(
        warmed, window,
        "the restart warm must fill the constants-derived walk window \
         (a flat 5,120 warm leaves the deepest retarget walk short)"
    );

    let anchor = engine
        .cache()
        .get(&plain_hash(ANCHOR))
        .cloned()
        .expect("anchor record warmed");
    get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(&anchor), engine.cache().records())
        .expect(
            "the epoch-turn retarget walk must compute over the warmed cache — every record it \
             reads is in the store",
        );
}

// The class E/F stage fixture: records [T-600, T] chained on the restamp hash convention with a
// hole at T-50, peak at T (set_peak's link walk stops at the hole — exactly the shape a lost
// candidate batch leaves). The staged block T+1 is a real body restamped and stripped to a
// non-transaction block, so the stage path runs straight into record derivation and its
// sub-epoch walk crosses the hole.
const STAGE_T: u32 = 6_000_228; // % 384 == 228: mid-sub-epoch, no epoch turn in the walk
const STAGE_BOTTOM: u32 = STAGE_T - 600;
const STAGE_HOLE: u32 = STAGE_T - 50;

fn stage_record(h: u32) -> BlockRecord {
    // Slot markers at three recent heights: warm-ancestry evidence (> 2 sub-slots and > 11
    // transaction blocks reachable) so the stage path engages the strict walks, as it does on a
    // real chain.
    let slot = h == STAGE_T - 5 || h == STAGE_T - 15 || h == STAGE_T - 25;
    walk_record(synth_hash_cd, h, true, slot, false)
}

fn synth_hash_cd(h: u32) -> Bytes32 {
    synth_hash(0xcd, h)
}

fn nontx_restamp(base: &FullBlock, h: u32) -> FullBlock {
    let mut b = common::restamp_block(base, h);
    b.transactions_generator = None;
    b.transactions_generator_ref_list = Vec::new();
    b.transactions_info = None;
    b.foliage_transaction_block = None;
    b.foliage.foliage_transaction_block_hash = None;
    b.foliage.foliage_transaction_block_signature = None;
    b
}

async fn stage_fixture() -> (Arc<SqliteStore>, Chaser<Arc<SqliteStore>, NativePrimitives>) {
    let path = common::unique_db_path();
    let store = Arc::new(SqliteStore::open(&path).await.expect("open"));
    let chain: Vec<BlockRecord> = (STAGE_BOTTOM..=STAGE_T)
        .filter(|h| *h != STAGE_HOLE)
        .map(stage_record)
        .collect();
    store.add_block_records(&chain).await.expect("seed chain");
    store
        .set_peak(&synth_hash_cd(STAGE_T))
        .await
        .expect("set peak");
    let engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
    let mut cfg = cfg();
    cfg.assume_valid = 7_000_000; // strip script/sig work; the walks under test still run
    let mut chaser = Chaser::new(engine, cfg);
    let warmed = chaser.warm_engine_cache().await.expect("warm");
    assert_eq!(
        warmed, 50,
        "the warm stops at the hole — only the suffix above it loads"
    );
    (store, chaser)
}

// Kill-point class E: records the driver's repair backfilled into the STORE must be readable by
// the stage walk WITHOUT a lockstep re-warm — the store is the fallback. A cache-only stage walk
// re-stages the identical window into the identical "block record not found" forever: the
// MissingRecord livelock this class pins.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stage_walk_falls_back_to_store_after_missing_record_backfill() {
    let (store, mut chaser) = stage_fixture().await;
    // The driver's repair lands the missing record in the store (an epoch-depth backfill row) —
    // the running engine's cache still has the 50-record suffix only.
    store
        .add_block_records(std::slice::from_ref(&stage_record(STAGE_HOLE)))
        .await
        .expect("backfill the hole");

    let base = common::load_full_block(5_000_000);
    let staged = nontx_restamp(&base, STAGE_T + 1);
    let err = chaser
        .follow_blocks(std::slice::from_ref(&staged))
        .await
        .expect_err("the synthetic body cannot fully validate");
    assert!(
        !err.is_missing_record(),
        "the stage walk must resolve backfilled records from the store — a missing-record error \
         here re-arms the identical repair forever (the restart-resume livelock), got: {err}"
    );
    // The fallback pulled the ancestry through the former hole into the walk cache…
    assert!(
        chaser
            .engine()
            .cache()
            .get(&synth_hash_cd(STAGE_HOLE))
            .is_some(),
        "the store fallback loads the repaired record into the walk cache"
    );
    // …and the exact walk that livelocked now computes over it.
    let anchor = chaser
        .engine()
        .cache()
        .get(&synth_hash_cd(STAGE_T))
        .cloned()
        .expect("anchor record");
    get_next_sub_slot_iters_and_difficulty(
        &MAINNET,
        true,
        Some(&anchor),
        chaser.engine().cache().records(),
    )
    .expect("the sub-epoch walk crosses the repaired hole");
}

// Kill-point class F (guard): a GENUINE record gap — the store is missing the record too — must
// still surface as the missing-record error, because that is the signal that re-arms the driver's
// header backfill. The fallback repairs cache/store misalignment; it must never mask a real gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn genuine_record_gap_still_surfaces_missing_record() {
    let (_store, mut chaser) = stage_fixture().await;
    let base = common::load_full_block(5_000_000);
    let staged = nontx_restamp(&base, STAGE_T + 1);
    let err = chaser
        .follow_blocks(std::slice::from_ref(&staged))
        .await
        .expect_err("the walk cannot cross a hole nothing holds");
    assert!(
        err.is_missing_record(),
        "a record absent from cache AND store must surface as missing-record (the driver's \
         backfill signal), got: {err}"
    );
}
