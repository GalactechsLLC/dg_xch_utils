//! Store-telemetry behavior against a real SQLite store: a COMMIT files its latency under the
//! band the store was in, the WAL file gauge sees the WAL, and the near-tip checkpointer records
//! its passes.

mod common;

use dg_xch_stores::BlockStore;
use std::sync::atomic::Ordering;
use std::time::Duration;

// A writer COMMIT is filed under the CURRENT phase: catch-up commits into commit_catch_up,
// near-tip commits into commit_near_tip. The WAL file gauge must also see a non-empty `-wal`
// after WAL-mode commits.
#[tokio::test]
async fn commit_latency_is_filed_under_the_current_phase() {
    let store = common::new_store().await;
    let t = store.telemetry().expect("sqlite records telemetry");
    assert_eq!(t.commit_catch_up.count.load(Ordering::Relaxed), 0);
    assert_eq!(t.commit_near_tip.count.load(Ordering::Relaxed), 0);
    assert_eq!(
        t.last_commit_unix.load(Ordering::Relaxed),
        0,
        "no commit yet"
    );

    // Catch-up band (the default): one batch commit.
    let records = common::load_records();
    let mut batch = store.begin().await.expect("begin");
    store
        .add_block_records_in(&mut batch, &records)
        .await
        .expect("records in batch");
    store.commit(batch).await.expect("commit");
    assert_eq!(t.commit_catch_up.count.load(Ordering::Relaxed), 1);
    assert_eq!(t.commit_near_tip.count.load(Ordering::Relaxed), 0);
    assert!(
        t.last_commit_unix.load(Ordering::Relaxed) > 0,
        "last-commit witness set"
    );

    // Near-tip band: the same commit files under the other label.
    store.set_near_tip(true);
    let mut batch = store.begin().await.expect("begin near tip");
    store
        .add_block_records_in(&mut batch, &records)
        .await
        .expect("records in batch");
    store.commit(batch).await.expect("commit near tip");
    assert_eq!(t.commit_catch_up.count.load(Ordering::Relaxed), 1);
    assert_eq!(t.commit_near_tip.count.load(Ordering::Relaxed), 1);

    // WAL-mode commits land in `-wal` first: the file-size gauge must see them.
    assert!(
        store.wal_bytes() > 0,
        "wal_bytes must observe a non-empty WAL after commits, got 0"
    );
}

// The near-tip-gated checkpointer records its passes: with near_tip=true its 1s tick runs the
// PASSIVE checkpoint and the telemetry must show completed passes and no errors. The 2.5s wait
// leaves margin for a slow CI runner.
#[tokio::test]
async fn checkpointer_records_passes_when_near_tip() {
    let store = common::new_store().await;
    let t = store.telemetry().expect("sqlite records telemetry");
    let records = common::load_records();
    let mut batch = store.begin().await.expect("begin");
    store
        .add_block_records_in(&mut batch, &records)
        .await
        .expect("records in batch");
    store.commit(batch).await.expect("commit");

    store.set_near_tip(true);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        t.checkpoint.count.load(Ordering::Relaxed) >= 1,
        "near-tip checkpointer must have completed at least one pass"
    );
    assert_eq!(
        t.checkpoint_errors_total.load(Ordering::Relaxed),
        0,
        "checkpoint passes must not error on a healthy store"
    );
}

#[tokio::test]
async fn checkpointer_periodically_probes_during_catch_up() {
    let store = common::new_store().await;
    let t = store.telemetry().expect("sqlite records telemetry");
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        t.checkpoint.count.load(Ordering::Relaxed) >= 1,
        "catch-up band must probe even without batch notifications"
    );
}

#[tokio::test]
async fn bulk_direct_coin_writes_are_checkpointed_without_batch_notifications() {
    use dg_xch_stores::CoinStore;

    const TRIGGER: u64 = 64 * 1024; // tiny in-test stand-in for the 128 MiB production trigger
    let path = common::unique_db_path();
    let store = dg_xch_stores::SqliteStore::open_with_wal_drain_trigger(&path, TRIGGER)
        .await
        .expect("open store");
    let t = store.telemetry().expect("sqlite records telemetry");
    assert!(!store.near_tip(), "bulk phase is the default");

    // Grow the WAL past the trigger with real coin applies (each lands in `-wal` first).
    let (adds, _) = common::load_adds_rems(5_000_000);
    let mut height = 10u32;
    while store.wal_bytes() <= TRIGGER {
        store
            .apply_block(height, 0, &adds, &[])
            .await
            .expect("apply");
        // Re-key the batch per height so every pass writes fresh rows, not no-op upserts.
        height += 1;
        assert!(height < 200, "the WAL must grow past the trigger");
    }
    let peak_wal = store.wal_bytes();
    assert!(peak_wal > TRIGGER);

    let mut drained = false;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if t.checkpoint.count.load(Ordering::Relaxed) >= 1 {
            drained = true;
            break;
        }
    }
    assert!(
        drained,
        "periodic drain must run within ~6 s (WAL {} bytes, was {peak_wal})",
        store.wal_bytes(),
    );
    assert_eq!(t.checkpoint_errors_total.load(Ordering::Relaxed), 0);

    // The writer stays free: a follow-up commit succeeds immediately on the drained store.
    store
        .apply_block(height, 0, &adds[..8], &[])
        .await
        .expect("post-drain apply");
}

// The writer's page-cache profile follows the sync phase: 256 MiB during bulk catch-up, sized to
// hold the dirty set of a cross-window batch commit without spilling it to the WAL, dropping back
// to 64 MiB at the tip where a single block's dirty set fits. Writer-only: the read pool and
// checkpointer keep the small default, so the bulk profile costs one connection's cache.
#[tokio::test]
async fn writer_cache_profile_follows_the_sync_phase() {
    let store = common::new_store().await;
    assert_eq!(
        store.writer_cache_size().await.expect("probe"),
        -262_144,
        "bulk (default) phase opens with the 256 MiB writer cache"
    );

    store.set_near_tip(true);
    let mut near = 0i64;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        near = store.writer_cache_size().await.expect("probe");
        if near == -65_536 {
            break;
        }
    }
    assert_eq!(near, -65_536, "near-tip flip shrinks the writer cache");

    store.set_near_tip(false);
    let mut bulk = 0i64;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bulk = store.writer_cache_size().await.expect("probe");
        if bulk == -262_144 {
            break;
        }
    }
    assert_eq!(
        bulk, -262_144,
        "falling back to bulk restores the big cache"
    );
}
