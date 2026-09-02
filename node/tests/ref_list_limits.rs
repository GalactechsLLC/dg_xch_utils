//! The generator back-reference list is peer-controlled input: every scan that walks it, and
//! body validation itself, must be bounded by the consensus cap
//! (`max_generator_ref_list_size`) BEFORE any per-entry work — a single block can otherwise
//! drive one store point-read per entry from the pre-validation scans.

mod common;

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const OVERSIZE: u32 = 10_000;

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 32,
        batch: 32,
        request_timeout: Duration::from_secs(20),
        assume_valid: 10_000_000,
    }
}

fn oversized_ref_block() -> FullBlock {
    let mut b = common::load_full_block(5_000_000);
    b.transactions_generator_ref_list = (1..=OVERSIZE).collect();
    b
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ref_scans_do_no_per_entry_work_past_the_consensus_cap() {
    let block = oversized_ref_block();
    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    let chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());

    let cap = MAINNET.max_generator_ref_list_size as u64;
    let window = vec![block];

    let before = telemetry.record_reads.load(Ordering::Relaxed);
    let extra = chaser.confirmed_ref_generators(&window).await;
    let reads = telemetry.record_reads.load(Ordering::Relaxed) - before;
    assert!(extra.is_empty(), "an over-cap ref list resolves nothing");
    assert!(
        reads <= cap,
        "confirmed_ref_generators walked {reads} store reads for an over-cap ref list \
         (cap {cap}) — per-entry work must stop at the consensus bound"
    );

    let before = telemetry.record_reads.load(Ordering::Relaxed);
    let missing = chaser.missing_ref_heights(&window).await;
    let reads = telemetry.record_reads.load(Ordering::Relaxed) - before;
    assert!(missing.is_empty(), "an over-cap ref list seeds no fetches");
    assert!(
        reads <= cap,
        "missing_ref_heights walked {reads} store reads for an over-cap ref list (cap {cap})"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_over_cap_ref_list_is_rejected_at_resolution_without_per_entry_reads() {
    use dg_xch_core::blockchain::sized_bytes::Bytes32;
    let mut block = oversized_ref_block();
    block.reward_chain_block.height = 100;
    block.reward_chain_block.weight = 1_000_000;
    block.foliage.prev_block_hash = Bytes32::default();

    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());
    let before = telemetry.record_reads.load(Ordering::Relaxed);
    let err = chaser
        .follow_blocks(std::slice::from_ref(&block))
        .await
        .expect_err("a ref list past max_generator_ref_list_size is invalid at every height");
    let reads = telemetry.record_reads.load(Ordering::Relaxed) - before;
    assert!(
        format!("{err:?}").contains("TooManyGeneratorRefs"),
        "wrong rejection for an over-cap ref list: {err:?}"
    );
    let cap = u64::from(MAINNET.max_generator_ref_list_size);
    assert!(
        reads <= cap + 16,
        "rejection cost {reads} store reads — the cap must fire before per-entry resolution"
    );
}
