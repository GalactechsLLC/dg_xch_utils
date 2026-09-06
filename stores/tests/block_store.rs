mod common;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use dg_xch_stores::{BlockStatus, BlockStore, SqliteStore};
use std::path::Path;

fn synth_hash(tag: u8, height: u32) -> Bytes32 {
    let mut h = [tag; 32];
    h[28..32].copy_from_slice(&height.to_be_bytes());
    Bytes32::from(h)
}

// A synthetic record cloned off a real mainnet template, re-linked into a chosen branch.
fn linked(template: &BlockRecord, tag: u8, height: u32, prev: Bytes32) -> BlockRecord {
    let mut r = template.clone();
    r.header_hash = synth_hash(tag, height);
    r.prev_hash = prev;
    r.height = height;
    r.weight = u128::from(height);
    r.total_iters = u128::from(height);
    r.sub_epoch_summary_included = None;
    r
}

// Every height carrying an in_main_chain=1 record, read straight from the schema (test-side probe).
async fn main_chain_heights(path: &Path) -> Vec<i64> {
    let url = format!("sqlite://{}?mode=ro", path.display());
    let pool = sqlx::SqlitePool::connect(&url)
        .await
        .expect("probe connect");
    let rows =
        sqlx::query("SELECT height FROM block_record WHERE in_main_chain = 1 ORDER BY height")
            .fetch_all(&pool)
            .await
            .expect("probe query");
    rows.iter()
        .map(|r| sqlx::Row::try_get::<i64, _>(r, "height").unwrap())
        .collect()
}

fn header_at(records: &[BlockRecord], height: u32) -> Bytes32 {
    records
        .iter()
        .find(|r| r.height == height)
        .map(|r| r.header_hash)
        .expect("height present in fixture")
}

async fn stored_with_records() -> (SqliteStore, Vec<BlockRecord>) {
    let records = common::load_records();
    let store = common::new_store().await;
    store.add_block_records(&records).await.unwrap();
    (store, records)
}

#[tokio::test]
async fn block_record_reloads_byte_for_byte() {
    let (store, records) = stored_with_records().await;
    let v = ChiaProtocolVersion::default();
    for original in records.iter().take(6) {
        let got = store
            .get_block_record(&original.header_hash)
            .await
            .unwrap()
            .expect("record present");
        assert_eq!(&got, original, "structural match");
        assert_eq!(
            got.to_bytes(v).unwrap(),
            original.to_bytes(v).unwrap(),
            "record byte match"
        );
    }
}

#[tokio::test]
async fn body_decompresses_to_the_exact_full_block() {
    let (store, records) = stored_with_records().await;
    let v = ChiaProtocolVersion::default();
    for height in [5_000_000u32, 5_000_004] {
        let fb = common::load_full_block(height);
        let hh = fb.header_hash().unwrap();
        assert_eq!(hh, header_at(&records, height), "fb id matches record id");

        let mut batch = store.begin().await.unwrap();
        store
            .append_many(&mut batch, std::slice::from_ref(&fb))
            .await
            .unwrap();
        store.commit(batch).await.unwrap();

        let got = store.get_block(&hh).await.unwrap().expect("body present");
        assert_eq!(got, fb, "full block structural match");
        assert_eq!(
            got.to_bytes(v).unwrap(),
            fb.to_bytes(v).unwrap(),
            "body byte match"
        );
    }
}

#[tokio::test]
async fn record_only_read_never_pages_the_body() {
    let (store, records) = stored_with_records().await;
    // Height 5000001 has a record but no body was ever appended.
    let hh = header_at(&records, 5_000_001);
    let rec = store.get_block_record(&hh).await.unwrap();
    assert!(rec.is_some(), "record read succeeds with no body stored");
    assert!(
        store.get_block(&hh).await.unwrap().is_none(),
        "no body present"
    );
}

// The sync-floor gauge source (sync-progress reporting): an empty store has no floor (absent — never
// a fake 0 that would read as "genesis-synced"), unconfirmed candidate records are not a floor, and
// once a chain is confirmed the floor is the lowest MAIN-CHAIN height — the store's own truth, rather
// than echoing whatever height the node was asked to sync from.
#[tokio::test]
async fn min_record_height_is_the_confirmed_floor() {
    let store = common::new_store().await;
    assert_eq!(
        store.min_record_height().await.unwrap(),
        None,
        "empty store: no floor"
    );

    let records = common::load_records();
    store.add_block_records(&records).await.unwrap();
    assert_eq!(
        store.min_record_height().await.unwrap(),
        None,
        "header-first candidates are not confirmed chain"
    );

    let top = header_at(&records, 5_000_029);
    store.set_peak(&top).await.unwrap();
    let expected = records.iter().map(|r| r.height).min().unwrap();
    assert_eq!(
        store.min_record_height().await.unwrap(),
        Some(expected),
        "floor = lowest main-chain height after confirm"
    );
}

#[tokio::test]
async fn get_by_height_returns_the_confirmed_record() {
    let (store, records) = stored_with_records().await;
    let top = header_at(&records, 5_000_029);
    store.set_peak(&top).await.unwrap();

    let got = store
        .get_block_record_by_height(5_000_010)
        .await
        .unwrap()
        .expect("confirmed record at height");
    assert_eq!(got.header_hash, header_at(&records, 5_000_010));

    let peak = store.get_peak().await.unwrap().expect("peak set");
    assert_eq!(peak, (top, 5_000_029));
}

#[tokio::test]
async fn validated_extension_fast_path_marks_only_the_supplied_chain() {
    let records = common::load_records();
    let template = &records[0];
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;

    let base = linked(template, 0, 100, Bytes32::from([0u8; 32]));
    let a1 = linked(template, 0xa1, 101, base.header_hash);
    let a2 = linked(template, 0xa2, 102, a1.header_hash);
    let sibling = linked(template, 0xb1, 101, base.header_hash);
    store
        .add_block_records(&[base.clone(), a1.clone(), a2.clone(), sibling])
        .await
        .unwrap();
    store.set_peak(&base.header_hash).await.unwrap();

    let mut batch = store.begin().await.unwrap();
    let links = store
        .extend_peak_in(&mut batch, &[a1.header_hash, a2.header_hash], a2.height)
        .await
        .unwrap();
    store.commit(batch).await.unwrap();

    assert_eq!(links, 2);
    assert_eq!(
        store.get_peak().await.unwrap(),
        Some((a2.header_hash, a2.height))
    );
    assert_eq!(
        store
            .get_block_record_by_height(a1.height)
            .await
            .unwrap()
            .unwrap()
            .header_hash,
        a1.header_hash,
        "the same-height sibling must remain a candidate"
    );
    assert_eq!(main_chain_heights(&path).await, vec![100, 101, 102]);
}

// T012a: a same-height sibling reorg must leave exactly one in_main_chain record per height, and
// get_block_record_by_height must return the new branch. Base B0@100; branch A (A1@101,A2@102) confirmed
// first, then heavier sibling branch B (B1@101,B2@102) takes the peak.
#[tokio::test]
async fn same_height_sibling_reorg_leaves_one_main_record_per_height() {
    let records = common::load_records();
    let template = &records[0];
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;

    let b0 = linked(template, 0, 100, Bytes32::from([0u8; 32]));
    let a1 = linked(template, 0xa1, 101, b0.header_hash);
    let a2 = linked(template, 0xa2, 102, a1.header_hash);
    let b1 = linked(template, 0xb1, 101, b0.header_hash);
    let b2 = linked(template, 0xb2, 102, b1.header_hash);
    store
        .add_block_records(&[b0.clone(), a1.clone(), a2.clone(), b1.clone(), b2.clone()])
        .await
        .unwrap();

    store.set_peak(&a2.header_hash).await.unwrap();
    store.set_peak(&b2.header_hash).await.unwrap();

    assert_eq!(
        main_chain_heights(&path).await,
        vec![100, 101, 102],
        "exactly one in_main_chain record per height after the sibling reorg"
    );
    assert_eq!(
        store
            .get_block_record_by_height(101)
            .await
            .unwrap()
            .unwrap()
            .header_hash,
        b1.header_hash,
        "height 101 resolves to the new branch"
    );
    assert_eq!(
        store
            .get_block_record_by_height(102)
            .await
            .unwrap()
            .unwrap()
            .header_hash,
        b2.header_hash,
        "height 102 resolves to the new branch tip"
    );
}

// T012a: a shorter-but-heavier reorg. Long branch A (A1..A3) confirmed to @103, then the peak flips to a
// shorter sibling B1@101 — heights above the new peak must carry no main-chain record, and the surviving
// old-branch sibling at 101 must be cleared.
#[tokio::test]
async fn shorter_heavier_reorg_clears_the_abandoned_branch() {
    let records = common::load_records();
    let template = &records[0];
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;

    let b0 = linked(template, 0, 100, Bytes32::from([0u8; 32]));
    let a1 = linked(template, 0xa1, 101, b0.header_hash);
    let a2 = linked(template, 0xa2, 102, a1.header_hash);
    let a3 = linked(template, 0xa3, 103, a2.header_hash);
    let b1 = linked(template, 0xb1, 101, b0.header_hash);
    store
        .add_block_records(&[b0.clone(), a1.clone(), a2.clone(), a3.clone(), b1.clone()])
        .await
        .unwrap();

    store.set_peak(&a3.header_hash).await.unwrap();
    store.set_peak(&b1.header_hash).await.unwrap();

    assert_eq!(
        main_chain_heights(&path).await,
        vec![100, 101],
        "old branch above the new (lower) peak is fully cleared"
    );
    assert_eq!(
        store
            .get_block_record_by_height(101)
            .await
            .unwrap()
            .unwrap()
            .header_hash,
        b1.header_hash,
        "height 101 resolves to the new shorter branch"
    );
    assert!(
        store
            .get_block_record_by_height(102)
            .await
            .unwrap()
            .is_none(),
        "no main-chain record above the new peak"
    );
    assert_eq!(
        store.get_peak().await.unwrap().unwrap(),
        (b1.header_hash, 101)
    );
}

#[tokio::test]
async fn empty_store_has_no_peak_and_default_status() {
    let store = common::new_store().await;
    assert!(
        store.get_peak().await.unwrap().is_none(),
        "fresh store has no peak"
    );
    let unknown = synth_hash(0x77, 999);
    assert_eq!(
        store.get_status(&unknown).await.unwrap(),
        BlockStatus::Unvalidated,
        "unknown block defaults to Unvalidated, not an error"
    );
}

// Durable per-block validation status round-trips through the block_record.status column:
// Unvalidated by default, and each set value reads back.
#[tokio::test]
async fn block_status_round_trips() {
    let (store, records) = stored_with_records().await;
    let hh = header_at(&records, 5_000_000);
    assert_eq!(
        store.get_status(&hh).await.unwrap(),
        BlockStatus::Unvalidated
    );
    for s in [
        BlockStatus::Validated,
        BlockStatus::Bypass,
        BlockStatus::Unvalidated,
    ] {
        store.set_status(&hh, s).await.unwrap();
        assert_eq!(store.get_status(&hh).await.unwrap(), s, "status persists");
    }
}

#[tokio::test]
async fn unassociated_reports_record_heights_lacking_bodies() {
    let (store, _records) = stored_with_records().await;

    // No bodies appended yet: the feed is the lowest heights, ordered and limited.
    let first5 = store.get_unassociated(5).await.unwrap();
    assert_eq!(
        first5,
        vec![5_000_000, 5_000_001, 5_000_002, 5_000_003, 5_000_004],
        "lowest five record-heights lacking a body, in order"
    );

    // Append the body at height 5_000_000; it drops out of the feed.
    let fb = common::load_full_block(5_000_000);
    let mut batch = store.begin().await.unwrap();
    store
        .append_many(&mut batch, std::slice::from_ref(&fb))
        .await
        .unwrap();
    store.commit(batch).await.unwrap();

    let after = store.get_unassociated(5).await.unwrap();
    assert!(
        !after.contains(&5_000_000),
        "a height with a stored body is no longer unassociated"
    );
    assert_eq!(
        after[0], 5_000_001,
        "the feed advances to the next body-less height"
    );
}

#[tokio::test]
async fn height_map_contiguity_via_block_store() {
    let (store, records) = stored_with_records().await;
    let top = header_at(&records, 5_000_029);
    store.set_peak(&top).await.unwrap();

    for h in 5_000_000u32..=5_000_029 {
        let got = store
            .get_block_record_by_height(h)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("height {h} resolves on the confirmed chain"));
        assert_eq!(
            got.height, h,
            "height->hash points at the record of that height"
        );
        assert_eq!(
            got.header_hash,
            header_at(&records, h),
            "resolves to the canonical hash"
        );
    }
    assert!(
        store
            .get_block_record_by_height(5_000_030)
            .await
            .unwrap()
            .is_none(),
        "no height resolves above the peak"
    );
}

#[tokio::test]
async fn sub_epoch_segments_round_trip_replace_and_survive_reopen() {
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;
    let ses_hash = Bytes32::from([7u8; 32]);

    assert!(
        store
            .get_sub_epoch_segments(&ses_hash)
            .await
            .unwrap()
            .is_none(),
        "unknown ses hash misses"
    );
    store
        .persist_sub_epoch_segments(&ses_hash, b"segments-v1")
        .await
        .unwrap();
    assert_eq!(
        store.get_sub_epoch_segments(&ses_hash).await.unwrap(),
        Some(b"segments-v1".to_vec())
    );
    store
        .persist_sub_epoch_segments(&ses_hash, b"segments-v2")
        .await
        .unwrap();
    assert_eq!(
        store.get_sub_epoch_segments(&ses_hash).await.unwrap(),
        Some(b"segments-v2".to_vec()),
        "re-persist replaces the row"
    );

    drop(store);
    let reopened = common::new_store_at(&path).await;
    assert_eq!(
        reopened.get_sub_epoch_segments(&ses_hash).await.unwrap(),
        Some(b"segments-v2".to_vec()),
        "segments survive a restart"
    );
}
