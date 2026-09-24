mod common;

use common::{LiveNodeApi, dial_source, seed_record_for, spawn_node_a};
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::{BlockStore, CoinStore};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::RwLock;

// A real mainnet block synced end-to-end over a loopback TLS socket through the full p2p
// seam (RequestBlocks → write-through → in-order confirm) leaves the coin store byte-equal to the reference
// additions for that block — the same reference the direct add_block path pins, now via the pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_sync_of_a_real_block_confirms_reference_coin_state() {
    let block = common::load_full_block(5_000_000);
    let (ref_adds, _ref_rems) = common::load_adds_rems(5_000_000);

    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, block.clone());
    let api = Arc::new(LiveNodeApi {
        blocks: RwLock::new(blocks),
    });
    let (port, server_run) = spawn_node_a(api).await;

    // Node B: seed the candidate header record so get_unassociated feeds the reservation window.
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let template = common::load_records()[0].clone();
    store
        .add_block_records(&[seed_record_for(&template, &block)])
        .await
        .expect("seed candidate");
    let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
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
    let peak = chaser
        .sync_range(std::slice::from_ref(&source))
        .await
        .expect("sync_range");
    assert_eq!(
        peak,
        Some((block.header_hash().unwrap(), 5_000_000)),
        "confirmed peak"
    );

    let store = chaser.engine().store();
    let gen_adds: Vec<&CoinRecord> = ref_adds.iter().filter(|c| !c.coinbase).collect();
    assert!(!gen_adds.is_empty());
    let names: Vec<Bytes32> = gen_adds.iter().map(|c| c.coin.name()).collect();
    let got = store.get_coin_records(&names).await.unwrap();
    let got_names: HashSet<Bytes32> = got.iter().map(|c| c.coin.name()).collect();
    for a in &gen_adds {
        assert!(
            got_names.contains(&a.coin.name()),
            "addition present after sync"
        );
    }
    assert_eq!(
        got.len(),
        gen_adds.len(),
        "exactly the reference additions were applied"
    );
    for c in &got {
        assert_eq!(c.confirmed_block_index, 5_000_000);
        assert!(!c.spent);
    }

    server_run.store(false, Ordering::Relaxed);
}
