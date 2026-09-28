mod common;

use common::{LiveNodeApi, dial_source, seed_record_for, spawn_node_a};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig, SyncError};
use dg_xch_stores::BlockStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::RwLock;

// The from-empty fast-sync REGRESSION GUARD. The live bug was: fast-sync validated the weight proof but the
// driver never proceeded to the body download/confirm, so `peak_height` stayed 0 forever. Weight-proof
// validation is proven separately (dg_xch_weight_proof end-to-end); THIS test pins the half that was broken —
// the driver-advancement seam `sync_headers-output -> sync_range -> confirm` MUST move the confirmed peak off
// zero over the REAL p2p RequestBlocks/RespondBlocks path. It also exercises the exact wire path whose bugs
// (introducer seeding, NewPeak tip tracking, RejectBlocks handling) we kept catching only in the deploy loop.
//
// The fixture seeds the real parent record and body, leaving the confirmed peak unset.
// The candidate's body must still download and confirm before the peak can advance.

fn one_peer_cfg() -> SyncConfig {
    SyncConfig {
        peers: 1,
        window: 4,
        batch: 1,
        request_timeout: Duration::from_secs(20),
        assume_valid: 0,
    }
}

// A chaser with no confirmed peak, a stored parent, and a candidate awaiting its body.
async fn empty_chaser_awaiting_body(
    block: &dg_xch_core::blockchain::full_block::FullBlock,
) -> Chaser<Arc<dg_xch_stores::SqliteStore>, NativePrimitives> {
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let template = common::load_records()[0].clone();
    store
        .add_block_records(&[seed_record_for(&template, block)])
        .await
        .expect("seed candidate header record");
    let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
    Chaser::new(engine, one_peer_cfg())
}

// GREEN target: from an empty store, the recent-body download + in-order confirm advances the confirmed peak
// from NONE (height 0) to the served block — over a real loopback TLS p2p socket. This fails on the "validates
// but never advances" regression because the confirmed peak would stay `None`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fast_sync_from_empty_advances_peak_over_real_p2p() {
    let block = common::load_full_block(5_000_000);

    // Node A: a real p2p node serving RequestBlocks for the block.
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, block.clone());
    let (port, server_run) = spawn_node_a(Arc::new(LiveNodeApi {
        blocks: RwLock::new(blocks),
    }))
    .await;

    // Node B: empty, awaiting the body. Peak starts unset — this is the "peak_height stays 0" starting state.
    let mut chaser = empty_chaser_awaiting_body(&block).await;
    assert_eq!(
        chaser.engine().store().get_peak().await.expect("get_peak"),
        None,
        "precondition: node B starts from an empty store (no peak)"
    );
    let confirmed_before = chaser.metrics().blocks_confirmed.load(Ordering::Relaxed);

    // Drive the download + confirm seam over the real dialed peer.
    let source = dial_source(port).await;
    let peak = chaser
        .sync_range(std::slice::from_ref(&source))
        .await
        .expect("sync_range must not error");

    // THE regression assertion: the peak advanced off zero to the confirmed block.
    assert_eq!(
        peak,
        Some((block.header_hash().unwrap(), 5_000_000)),
        "confirmed peak must ADVANCE to the served block, not stay at 0"
    );
    assert_eq!(
        chaser
            .metrics()
            .blocks_confirmed
            .load(Ordering::Relaxed)
            .saturating_sub(confirmed_before),
        1,
        "exactly one block was confirmed into the chain"
    );

    server_run.store(false, Ordering::Relaxed);
}

// Regression guard: confirming a body at a weight-proof checkpoint with partial ancestry
// must KEEP the headers-first candidate record's attested
// sub_slot_iters, never fabricate `sub_slot_iters_starting` into the confirmed record. Records
// are epoch-adjusted at creation, and when the deep ancestry is absent the proof's summaries
// substitute; the candidate record carries that attested epoch value here. Fabrication would
// poison the anchor record with the genesis constant and every descendant would inherit it —
// sporadic `Required iters N is not below the sp interval` rejections just after sub-epoch
// boundaries
// (mainnet 9,141,129 and 9,143,058: true bound ssi/64 = 8,912,896 vs poisoned bound 2,097,152).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anchor_confirm_keeps_the_candidate_epoch_sub_slot_iters() {
    let block = common::load_full_block(5_000_000);
    let candidate_ssi = common::load_records()[0].sub_slot_iters;
    assert_ne!(
        candidate_ssi, MAINNET.sub_slot_iters_starting,
        "precondition: the real epoch ssi differs from the genesis constant (else the test proves nothing)"
    );

    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, block.clone());
    let (port, server_run) = spawn_node_a(Arc::new(LiveNodeApi {
        blocks: RwLock::new(blocks),
    }))
    .await;

    let mut chaser = empty_chaser_awaiting_body(&block).await;
    let source = dial_source(port).await;
    chaser
        .sync_range(std::slice::from_ref(&source))
        .await
        .expect("sync_range must not error");

    let confirmed = chaser
        .engine()
        .store()
        .get_block_record(&block.header_hash().unwrap())
        .await
        .expect("get_block_record")
        .expect("confirmed record exists");
    assert_eq!(
        confirmed.sub_slot_iters, candidate_ssi,
        "the confirmed record must carry the candidate's weight-proof-attested epoch sub_slot_iters, \
         not a fabricated sub_slot_iters_starting"
    );

    server_run.store(false, Ordering::Relaxed);
}

// GUARD for the RejectBlocks path (the class of bug that manifested as "no peer can serve height N"): when the
// only peer cannot serve a requested height, the pipeline must FAIL FAST and BOUNDED — surfacing an error and
// leaving the peak unadvanced — never hang until the request timeout on every attempt. Before the reject
// handling + failure-budget fix, this range would stall ~request_timeout per attempt; the test's short wall
// bound is what proves it now fails fast.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unservable_range_fails_fast_and_does_not_advance() {
    let block = common::load_full_block(5_000_000);

    // Node A serves height 5_000_000 only; node B will be told (via its candidate record) to fetch a DIFFERENT
    // height A does not have, forcing a RejectBlocks.
    let mut blocks = HashMap::new();
    blocks.insert(5_000_000u32, block.clone());
    let (port, server_run) = spawn_node_a(Arc::new(LiveNodeApi {
        blocks: RwLock::new(blocks),
    }))
    .await;

    // Seed a candidate for a height A does NOT serve (its header_hash still keys distinctly; A rejects it).
    let store = common::new_store().await;
    let template = common::load_records()[0].clone();
    let mut phantom = seed_record_for(&template, &block);
    phantom.height = 4_999_999; // A has no block at this height -> RejectBlocks on request
    store
        .add_block_records(&[phantom])
        .await
        .expect("seed phantom candidate");
    let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
    let mut chaser = Chaser::new(engine, one_peer_cfg());

    let source = dial_source(port).await;
    // Bounded wall clock chosen BELOW one request_timeout (20s): a reject resolves in
    // milliseconds, so the whole failure budget (5 attempts * 250ms backoff) finishes in ~1.5s.
    // Waiting for the per-request timeout on every attempt (a reject never matching a
    // RespondBlocks-only waiter) could not finish inside this bound.
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        chaser.sync_range(std::slice::from_ref(&source)),
    )
    .await
    .expect(
        "sync_range must return fast on an unservable range, never hang past a request timeout",
    );

    assert!(
        matches!(result, Err(SyncError::Exhausted(_))),
        "an unservable range must surface Exhausted, got {result:?}"
    );
    assert_eq!(
        chaser.engine().store().get_peak().await.expect("get_peak"),
        None,
        "peak must not advance when the range cannot be served"
    );

    server_run.store(false, Ordering::Relaxed);
}
