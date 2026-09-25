use super::*;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::consensus::constants::MAINNET;
#[cfg(feature = "mmap")]
use dg_xch_stores::MmapStore;
use dg_xch_stores::SqliteStore;
use std::time::{SystemTime, UNIX_EPOCH};

fn h32(n: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    Bytes32::from(b)
}

fn record(height: u32) -> BlockRecord {
    BlockRecord {
        header_hash: h32(height),
        prev_hash: h32(height.wrapping_sub(1)),
        height,
        weight: 7 * u128::from(height),
        total_iters: 10_000_000 * u128::from(height),
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
        prev_transaction_block_height: height.wrapping_sub(1),
        timestamp: Some(1_000 + u64::from(height)),
        prev_transaction_block_hash: None,
        fees: None,
        reward_claims_incorporated: None,
        finished_challenge_slot_hashes: None,
        finished_infused_challenge_slot_hashes: None,
        finished_reward_slot_hashes: None,
        sub_epoch_summary_included: None,
    }
}

fn records(range: impl Iterator<Item = u32>) -> Vec<BlockRecord> {
    range.map(record).collect()
}

async fn sqlite_store() -> SqliteStore {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "resume_floor_{}_{nanos}.sqlite",
        std::process::id()
    ));
    SqliteStore::open(&path).await.expect("open sqlite")
}

#[cfg(feature = "mmap")]
async fn mmap_store() -> MmapStore {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("resume_floor_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    MmapStore::open(&dir).await.expect("open mmap")
}

// The anchored-leg shape a deployed anchored store is in: the confirmed span [A, P] is
// main-chain (set_peak ran while nothing below A existed), then the epoch-depth backfill
// landed CANDIDATE records [L, A). A restart must see those candidates as floor-satisfying —
// by-height they are invisible. (Postgres shares sqlite's `in_main_chain = 1` by-height SQL
// verbatim — stores/src/postgres/block.rs — and the walk goes through the same generic
// `BlockStore::get_block_record`, so sqlite+mmap coverage carries; the postgres contract
// suite pins the by-hash/by-height visibility semantics themselves.)
async fn anchored_shape<S: BlockStore + Send + Sync>(store: &S) {
    const L: u32 = 9_400;
    const A: u32 = 9_700;
    const P: u32 = 10_000;
    store
        .add_block_records(&records(A..=P))
        .await
        .expect("seed confirmed span");
    store.set_peak(&h32(P)).await.expect("set peak");
    // The backfill lands AFTER the peak exists — candidates, never flipped main.
    store
        .add_block_records(&records(L..A))
        .await
        .expect("seed backfilled candidates");
}

async fn assert_anchored_reached<S: BlockStore + Send + Sync>(store: &S) {
    const P: u32 = 10_000;
    const NEEDED_LOW: u32 = 9_500;
    let got = measure_record_floor(store, h32(P), P, NEEDED_LOW)
        .await
        .expect("measure");
    assert!(
        matches!(got, RecordFloor::Reached { floor } if floor <= NEEDED_LOW),
        "backfilled candidate records must satisfy the floor walk \
         (no weight-proof refetch on a clean restart), got {got:?}"
    );
}

#[tokio::test]
async fn anchored_backfilled_candidates_satisfy_the_floor_on_sqlite() {
    let store = sqlite_store().await;
    anchored_shape(&store).await;
    assert_anchored_reached(&store).await;
}

#[cfg(feature = "mmap")]
#[tokio::test]
async fn anchored_backfilled_candidates_satisfy_the_floor_on_mmap() {
    let store = mmap_store().await;
    anchored_shape(&store).await;
    assert_anchored_reached(&store).await;
}

// The mid-span-hole shape: records main-chain-visible on BOTH sides of a single
// missing record (a lost mmap link batch / a dropped Postgres candidate batch, later peaks
// landed). The by-height floor search converges below the hole and reports "nothing to
// repair" while every stage walk from the peak dies crossing the hole — the livelock. The
// walk must report Broken exactly at the hole so the repair backfills THE HOLE.
const HOLE_P: u32 = 1_024;
const HOLE_H: u32 = HOLE_P - 3; // never a binary-search probe point on the all-present descent
async fn hole_shape<S: BlockStore + Send + Sync>(store: &S) {
    store
        .add_block_records(&records(0..HOLE_H))
        .await
        .expect("seed below hole");
    store.set_peak(&h32(HOLE_H - 1)).await.expect("peak below");
    store
        .add_block_records(&records(HOLE_H + 1..=HOLE_P))
        .await
        .expect("seed above hole");
    // set_peak's prev-hash link walk breaks at the hole: [H+1, P] flip main, H stays missing.
    store.set_peak(&h32(HOLE_P)).await.expect("peak above");
}

async fn assert_hole_detected_and_converges<S: BlockStore + Send + Sync>(store: &S) {
    const NEEDED_LOW: u32 = 100;
    let got = measure_record_floor(store, h32(HOLE_P), HOLE_P, NEEDED_LOW)
        .await
        .expect("measure");
    assert_eq!(
        got,
        RecordFloor::Broken { stop: HOLE_H + 1 },
        "a mid-span hole above the by-height floor must be DETECTED (Broken at the hole), \
         not read as nothing-to-repair"
    );
    // The repair backfills the hole (headers anchored at `stop` cover it); once the record
    // lands, the same walk must converge to Reached.
    store
        .add_block_records(&records(HOLE_H..=HOLE_H))
        .await
        .expect("backfill the hole");
    let after = measure_record_floor(store, h32(HOLE_P), HOLE_P, NEEDED_LOW)
        .await
        .expect("measure after backfill");
    assert!(
        matches!(after, RecordFloor::Reached { floor } if floor <= NEEDED_LOW),
        "after the hole is backfilled the floor walk must converge, got {after:?}"
    );
}

#[tokio::test]
async fn mid_span_hole_is_detected_and_repair_converges_on_sqlite() {
    let store = sqlite_store().await;
    hole_shape(&store).await;
    assert_hole_detected_and_converges(&store).await;
}

#[cfg(feature = "mmap")]
#[tokio::test]
async fn mid_span_hole_is_detected_and_repair_converges_on_mmap() {
    let store = mmap_store().await;
    hole_shape(&store).await;
    assert_hole_detected_and_converges(&store).await;
}

// A clean full-history chain short-circuits.
#[tokio::test]
async fn clean_genesis_chain_reaches_the_floor_on_sqlite() {
    let store = sqlite_store().await;
    store
        .add_block_records(&records(0..=600))
        .await
        .expect("seed");
    store.set_peak(&h32(600)).await.expect("peak");
    let got = measure_record_floor(&store, h32(600), 600, 300)
        .await
        .expect("measure");
    assert!(
        matches!(got, RecordFloor::Reached { floor } if floor <= 300),
        "clean chain must reach the floor, got {got:?}"
    );
}

// The walk is bounded even against a corrupt self-referential chain: it terminates Broken
// rather than spinning (bound = the needed span + slack). No `set_peak` here — the store's
// own peak walk assumes an acyclic chain; the floor walk takes the peak explicitly and must
// stay bounded regardless.
#[tokio::test]
async fn corrupt_prev_hash_cycle_terminates_broken() {
    let store = sqlite_store().await;
    let peak = record(500);
    let mut cyc = record(499);
    cyc.prev_hash = cyc.header_hash; // self-loop below the peak
    store
        .add_block_records(&[peak.clone(), cyc])
        .await
        .expect("seed cycle");
    let got = measure_record_floor(&store, peak.header_hash, 500, 100)
        .await
        .expect("measure");
    assert!(
        matches!(got, RecordFloor::Broken { .. }),
        "a prev-hash cycle must terminate Broken, got {got:?}"
    );
}
