use super::*;
use crate::config::Backend;
use crate::node::open_backend;
use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sub_epoch_summary::SubEpochSummary;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::consensus::difficulty_adjustment::get_next_sub_slot_iters_and_difficulty;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

async fn store_walk_records_map<S: BlockStore + Send + Sync>(
    store: &S,
    tip: Bytes32,
    depth: u32,
) -> HashMap<Bytes32, BlockRecord> {
    let mut map = HashMap::new();
    let mut hash = tip;
    for _ in 0..depth {
        let Ok(Some(rec)) = store.get_block_record(&hash).await else {
            break;
        };
        let prev = rec.prev_hash;
        let height = rec.height;
        map.insert(hash, rec);
        if height == 0 {
            break;
        }
        hash = prev;
    }
    map
}

fn h32(n: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    Bytes32::from(b)
}

// Fork-branch hashes: same heights, distinct identities.
fn h32f(n: u32) -> Bytes32 {
    let mut b = [0u8; 32];
    b[..4].copy_from_slice(&n.to_be_bytes());
    b[31] = 0xFF;
    Bytes32::from(b)
}

fn ses() -> SubEpochSummary {
    SubEpochSummary {
        prev_subepoch_summary_hash: Bytes32::default(),
        reward_chain_hash: Bytes32::default(),
        num_blocks_overflow: 0,
        new_difficulty: None,
        new_sub_slot_iters: None,
    }
}

fn record(height: u32, hash: Bytes32, prev: Bytes32, with_ses: bool) -> BlockRecord {
    BlockRecord {
        header_hash: hash,
        prev_hash: prev,
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
        finished_reward_slot_hashes: if with_ses {
            Some(vec![Bytes32::default()])
        } else {
            None
        },
        sub_epoch_summary_included: if with_ses { Some(ses()) } else { None },
    }
}

// Epoch boundary under test: B = 2000 * 4608. Main chain [B-5800, B+120], every record a
// transaction block; the previous epoch's SES two blocks past its surpass (B-4608+2) and the
// epoch SES three blocks past B — the mainnet shape (the can_finish walks exit at an SES).
const B: u32 = 2000 * 4608;
const CHAIN_BOTTOM: u32 = B - 5800;
const CHAIN_TOP: u32 = B + 120;

fn main_chain() -> Vec<BlockRecord> {
    (CHAIN_BOTTOM..=CHAIN_TOP)
        .map(|h| {
            let with_ses = h == B - MAINNET.epoch_blocks + 2 || h == B + 3;
            record(h, h32(h), h32(h.wrapping_sub(1)), with_ses)
        })
        .collect()
}

async fn seeded_store() -> Arc<dg_xch_stores::SqliteStore> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "record_window_{}_{nanos}.sqlite",
        std::process::id()
    ));
    let store = open_backend(&Backend::Sqlite(path)).await.expect("store");
    store
        .add_block_records(&main_chain())
        .await
        .expect("seed records");
    store
}

fn fresh_window() -> Mutex<BlockRecordCache> {
    Mutex::new(BlockRecordCache::new(record_window_capacity(&MAINNET)))
}

async fn rec_at(store: &dg_xch_stores::SqliteStore, hash: Bytes32) -> BlockRecord {
    store
        .get_block_record(&hash)
        .await
        .expect("store read")
        .expect("record present")
}

// The capacity bound is proven against the real depth function, not the arithmetic comment:
// every anchor offset across two full epochs fits with the trailing-anchor slack to spare.
#[test]
fn capacity_covers_every_depth_with_trailing_slack() {
    let cap = record_window_capacity(&MAINNET);
    let slack = 2 * MAINNET.max_sub_slot_blocks;
    let base = 2000 * MAINNET.epoch_blocks;
    let mut max_depth = 0;
    for h in base..base + 2 * MAINNET.epoch_blocks {
        let depth = difficulty_record_depth(&MAINNET, h);
        max_depth = max_depth.max(depth);
        assert!(
            (depth + slack) as usize <= cap,
            "offset {}: depth {depth} + slack {slack} exceeds capacity {cap}",
            h % MAINNET.epoch_blocks
        );
    }
    // The epoch-turn regime is the deepest walk.
    assert_eq!(max_depth, 5_503);
}

#[tokio::test]
async fn store_reads_collapse_from_depth_per_call_to_head_delta() {
    let store = seeded_store().await;
    let window = fresh_window();
    let metrics = SyncMetrics::default();

    let anchor_cold = rec_at(store.as_ref(), h32(B + 10)).await;
    let depth_cold = difficulty_record_depth(&MAINNET, B + 10);
    assert_eq!(depth_cold, 5_131, "epoch-turn regime depth at offset 10");
    let map = windowed_records_map(&window, store.as_ref(), &metrics, &MAINNET, &anchor_cold).await;
    assert_eq!(map.len(), depth_cold as usize);
    let cold_reads = metrics
        .difficulty_window_store_reads
        .load(Ordering::Relaxed);
    assert_eq!(
        cold_reads,
        u64::from(depth_cold),
        "cold start pays the full window once"
    );

    // One follow window later: only the 32 new head records are fetched.
    let anchor_warm = rec_at(store.as_ref(), h32(B + 42)).await;
    let map = windowed_records_map(&window, store.as_ref(), &metrics, &MAINNET, &anchor_warm).await;
    assert_eq!(
        map.len(),
        difficulty_record_depth(&MAINNET, B + 42) as usize
    );
    let after_delta = metrics
        .difficulty_window_store_reads
        .load(Ordering::Relaxed);
    assert_eq!(
        after_delta - cold_reads,
        32,
        "warm call fetches only the head delta"
    );

    // Same-peak re-serve (the sp-inbox tick between peaks): zero store reads.
    let map2 =
        windowed_records_map(&window, store.as_ref(), &metrics, &MAINNET, &anchor_warm).await;
    assert_eq!(map2, map);
    assert_eq!(
        metrics
            .difficulty_window_store_reads
            .load(Ordering::Relaxed),
        after_delta,
        "re-serving the same anchor touches the store zero times"
    );
}

#[tokio::test]
async fn map_and_ssi_difficulty_are_byte_identical_across_the_boundary() {
    let store = seeded_store().await;
    let warmed = fresh_window();
    let metrics = SyncMetrics::default();
    for offset_anchor in [B - 1, B, B + 1, B + 42, B + 100] {
        let anchor = rec_at(store.as_ref(), h32(offset_anchor)).await;
        let depth = difficulty_record_depth(&MAINNET, anchor.height);
        let oracle = store_walk_records_map(store.as_ref(), anchor.header_hash, depth).await;

        let cold = fresh_window();
        let cold_map =
            windowed_records_map(&cold, store.as_ref(), &metrics, &MAINNET, &anchor).await;
        assert_eq!(
            cold_map, oracle,
            "cold map diverged at anchor {offset_anchor}"
        );

        let warm_map =
            windowed_records_map(&warmed, store.as_ref(), &metrics, &MAINNET, &anchor).await;
        assert_eq!(
            warm_map, oracle,
            "warm map diverged at anchor {offset_anchor}"
        );

        let want = get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(&anchor), &oracle)
            .expect("oracle computes");
        let got = get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(&anchor), &warm_map)
            .expect("windowed map computes");
        assert_eq!(
            got, want,
            "SSI/difficulty diverged at anchor {offset_anchor}"
        );
    }
}

// Cache correctness under a reorg ACROSS the epoch boundary: after the window is warmed on the
// main chain past B, a fork from B-20 overwrites heights spanning the boundary. The walk must
// serve the fork's ancestry (fresh hashes -> store), never a superseded main-chain record, and
// the map + SSI/difficulty must match the pure store walk exactly.
#[tokio::test]
async fn reorg_across_the_epoch_boundary_never_serves_the_stale_branch() {
    let store = seeded_store().await;
    let window = fresh_window();
    let metrics = SyncMetrics::default();

    // Warm on the main chain, anchored past the boundary.
    let main_anchor = rec_at(store.as_ref(), h32(B + 50)).await;
    windowed_records_map(&window, store.as_ref(), &metrics, &MAINNET, &main_anchor).await;

    // The fork: parent B-21 (main), replacement records for B-20..=B+60 under fork hashes,
    // with the epoch SES on the fork at B+4 (a different post-boundary shape than main's B+3).
    let fork: Vec<BlockRecord> = (B - 20..=B + 60)
        .map(|h| {
            let prev = if h == B - 20 { h32(h - 1) } else { h32f(h - 1) };
            record(h, h32f(h), prev, h == B + 4)
        })
        .collect();
    store.add_block_records(&fork).await.expect("seed fork");

    let fork_anchor = rec_at(store.as_ref(), h32f(B + 60)).await;
    let depth = difficulty_record_depth(&MAINNET, fork_anchor.height);
    let oracle = store_walk_records_map(store.as_ref(), fork_anchor.header_hash, depth).await;
    let map = windowed_records_map(&window, store.as_ref(), &metrics, &MAINNET, &fork_anchor).await;
    assert_eq!(map, oracle, "post-reorg map diverged from the store walk");

    // No superseded main-chain record may appear at the overwritten heights.
    for h in B - 20..=B + 50 {
        assert!(
            !map.contains_key(&h32(h)),
            "stale main-chain record at height {h} leaked into the post-reorg map"
        );
    }
    let want = get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(&fork_anchor), &oracle)
        .expect("oracle computes");
    let got = get_next_sub_slot_iters_and_difficulty(&MAINNET, true, Some(&fork_anchor), &map)
        .expect("windowed map computes");
    assert_eq!(got, want, "post-reorg SSI/difficulty diverged");
}
