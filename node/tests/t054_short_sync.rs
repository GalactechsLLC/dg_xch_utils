mod common;

use common::{LiveNodeApi, dial_source, spawn_node_a};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

// One mainnet block time (600 s sub-slot / 32 blocks ≈ 18.75 s) — short sync must track a new peak inside it.
const ONE_BLOCK_TIME: Duration = Duration::from_millis(18_750);

// Short sync follows a peer's newly-announced tip. Node A starts with no relevant tip; node B is
// caught up (peak None). Node A advances its peak (a block becomes its tip); node B follows and confirms it
// within one block time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn node_b_follows_node_a_new_peak_within_one_block_time() {
    let block = common::load_full_block(5_000_000);

    // Node A starts empty — nothing to follow yet.
    let api = Arc::new(LiveNodeApi {
        blocks: RwLock::new(HashMap::new()),
    });
    let (port, server_run) = spawn_node_a(api.clone()).await;

    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let engine = Engine::new(store, NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(
        engine,
        SyncConfig {
            peers: 1,
            window: 4,
            batch: 1,
            request_timeout: Duration::from_secs(20),
            assume_valid: 0,
        },
    );
    let source = dial_source(port).await;

    // Caught up: nothing confirmed yet.
    assert!(chaser.engine().store().get_peak().await.unwrap().is_none());

    // Node A advances its peak: the block becomes its tip (a new_peak the follower will chase).
    api.blocks.write().await.insert(5_000_000, block.clone());

    let t0 = Instant::now();
    let peak = chaser
        .follow_to(&source, 5_000_000, 5_000_000)
        .await
        .expect("follow the new peak");
    let elapsed = t0.elapsed();

    assert_eq!(
        peak,
        Some((block.header_hash().unwrap(), 5_000_000)),
        "node B confirmed node A's new peak"
    );
    assert!(
        elapsed < ONE_BLOCK_TIME,
        "tracked the new peak in {elapsed:?}, within one block time ({ONE_BLOCK_TIME:?})"
    );
    assert_eq!(chaser.metrics().blocks_confirmed.load(Ordering::Relaxed), 1);

    server_run.store(false, Ordering::Relaxed);
}
