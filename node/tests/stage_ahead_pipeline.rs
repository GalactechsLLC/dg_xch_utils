//! The stage-ahead pipeline's correctness pins.
//!
//! The daemon overlaps window N's vdf/sig drain (a spawned pure-CPU task) with window N+1's
//! staging, confirming strictly in order. Two properties make that safe, and these tests pin
//! both against the serial path on a real store:
//!
//! 1. Staging window N+1 BEFORE window N confirms (against N's overlay entries alone) yields
//!    the same confirmed chain as the fully serial follow — cross-window staged reads need no
//!    committed state.
//! 2. Dry staging writes NOTHING: a window staged and abandoned (the crash shape) leaves the
//!    store byte-identical to never having seen it, and the window replays cleanly.

mod common;

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::sync::drain_staged_window;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE_WEIGHT: u128 = 1_000_000;

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
async fn staging_the_next_window_before_the_confirm_matches_the_serial_path() {
    let base = common::load_full_block(5_000_000);
    let w1 = build_chain(&base, 100, 131, common::synth_hash(0xaa, 99));
    let w2 = build_chain(&base, 132, 163, w1.last().unwrap().header_hash().unwrap());

    // Serial reference: the ordinary follow, window by window.
    let serial_store = Arc::new(common::new_store().await);
    serial_store.set_near_tip(false);
    let mut serial = Chaser::new(Engine::new(serial_store, NativePrimitives, MAINNET), cfg());
    let serial_p1 = serial.follow_blocks(&w1).await.expect("w1 confirms");
    let serial_p2 = serial.follow_blocks(&w2).await.expect("w2 confirms");

    // Pipelined order: stage(w1) -> stage(w2) -> drain(w1) -> confirm(w1) -> drain(w2) ->
    // confirm(w2). Window 2 stages against window 1's OVERLAY only — nothing of w1 is
    // committed yet.
    let piped_store = Arc::new(common::new_store().await);
    piped_store.set_near_tip(false);
    let mut piped = Chaser::new(Engine::new(piped_store, NativePrimitives, MAINNET), cfg());
    let mut s1 = piped
        .stage_window_pre(w1.clone(), None)
        .await
        .expect("w1 stages");
    let mut s2 = piped
        .stage_window_pre(w2.clone(), None)
        .await
        .expect("w2 stages against w1's uncommitted overlay");
    let constants = MAINNET;
    let v1 = drain_staged_window(&NativePrimitives, &constants, s1.take_drain_input());
    let p1 = piped
        .confirm_window_pre(s1, v1)
        .await
        .expect("w1 confirms")
        .peak;
    let v2 = drain_staged_window(&NativePrimitives, &constants, s2.take_drain_input());
    let p2 = piped
        .confirm_window_pre(s2, v2)
        .await
        .expect("w2 confirms")
        .peak;

    assert_eq!(p1, serial_p1, "window 1's confirmed peak diverges");
    assert_eq!(p2, serial_p2, "window 2's confirmed peak diverges");
    assert_eq!(
        p2,
        Some((w2.last().unwrap().header_hash().unwrap(), 163)),
        "the pipelined chain confirmed to its tip"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pipelined_windows_still_cost_one_writer_transaction_each() {
    let base = common::load_full_block(5_000_000);
    let w1 = build_chain(&base, 100, 107, common::synth_hash(0xab, 99));
    let w2 = build_chain(&base, 108, 115, w1.last().unwrap().header_hash().unwrap());

    let store = Arc::new(common::new_store().await);
    let telemetry = store.telemetry().expect("sqlite store exposes telemetry");
    store.set_near_tip(false);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());

    let before = telemetry.commit_catch_up.count.load(Ordering::Relaxed);
    let mut s1 = chaser.stage_window_pre(w1, None).await.expect("w1 stages");
    let mut s2 = chaser.stage_window_pre(w2, None).await.expect("w2 stages");
    let constants = MAINNET;
    let v1 = drain_staged_window(&NativePrimitives, &constants, s1.take_drain_input());
    chaser
        .confirm_window_pre(s1, v1)
        .await
        .expect("w1 confirms");
    let v2 = drain_staged_window(&NativePrimitives, &constants, s2.take_drain_input());
    chaser
        .confirm_window_pre(s2, v2)
        .await
        .expect("w2 confirms");
    let commits = telemetry.commit_catch_up.count.load(Ordering::Relaxed) - before;
    assert_eq!(
        commits, 2,
        "each pipelined window must still cost exactly ONE writer transaction (archive + coins \
         + peak together); staging must never open its own"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abandoned_dry_staged_window_leaves_no_trace_and_replays() {
    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, 100, 107, common::synth_hash(0xac, 99));

    let store = Arc::new(common::new_store().await);
    store.set_near_tip(false);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());
    let staged = chaser
        .stage_window_pre(chain.clone(), None)
        .await
        .expect("window stages");
    let first_hash = chain.first().unwrap().header_hash().unwrap();
    // The crash shape: the window is staged, its drain never lands, the process dies. Nothing
    // may have reached the store — no peak, no archive rows.
    assert_eq!(
        chaser.engine().store().get_peak().await.expect("peak read"),
        None,
        "dry staging must not advance the durable peak"
    );
    assert!(
        chaser
            .engine()
            .store()
            .get_block_record(&first_hash)
            .await
            .expect("record read")
            .is_none(),
        "dry staging must not persist archive rows"
    );
    drop(staged);
    chaser.clear_staged_overlay();

    // Resume: the same window re-fetches and follows cleanly.
    let peak = chaser.follow_blocks(&chain).await.expect("replay confirms");
    assert_eq!(
        peak,
        Some((chain.last().unwrap().header_hash().unwrap(), 107)),
        "the abandoned window replays to its tip"
    );
}

// A provided precompute must never widen what validation accepts: a block whose committed ref
// list names a height with no generator anywhere on the chain is invalid inline, and must stay
// invalid when a precompute (built without that ref) is handed in — the engine's refs-digest
// check drops the mismatched precompute and the inline path rejects.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_precompute_cannot_smuggle_past_an_unresolvable_ref() {
    use dg_xch_core::consensus::block_generator::transactions_generator_refs_root;
    use dg_xch_node::engine::{PrecomputedBody, precompute_refs_digest, run_body_expensive};

    let base = common::load_full_block(5_000_000);
    let mut chain = build_chain(&base, 100, 107, common::synth_hash(0xad, 99));
    let victim_height = 107;
    {
        let victim = chain.last_mut().unwrap();
        victim.transactions_generator_ref_list = vec![99];
        victim
            .transactions_info
            .as_mut()
            .expect("fixture tx block carries transactions_info")
            .generator_refs_root = transactions_generator_refs_root(&[99]).expect("refs root");
    }
    let victim = chain.last().unwrap().clone();
    let (conds, verified) =
        run_body_expensive(&NativePrimitives, &MAINNET, &victim, &[], false).expect("body runs");
    let pre = PrecomputedBody {
        conds,
        agg_sig_verified: verified,
        refs_digest: precompute_refs_digest(victim_height, &[], false),
    };
    let provided = std::collections::HashMap::from([(victim_height, pre)]);

    let store = Arc::new(common::new_store().await);
    store.set_near_tip(false);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());
    let err = chaser
        .follow_blocks_reporting_pre(&chain, Some(provided))
        .await
        .expect_err("a ref to a generator-less height is invalid, precompute or not");
    assert!(
        format!("{err:?}").contains("GeneratorRefHasNoGenerator"),
        "wrong rejection: {err:?}"
    );
}

// A store failure inside the confirm must retract the staged overlay on its way out: the
// unconfirmed window re-stages wholesale after the queue reset, and a stale height-keyed
// overlay entry from the abandoned attempt must not shadow anything that stages later.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_confirm_store_failure_retracts_the_staged_overlay() {
    use common::fault::FaultStore;
    use dg_xch_node::sync::drain_staged_window;

    let base = common::load_full_block(5_000_000);
    let chain = build_chain(&base, 100, 107, common::synth_hash(0xae, 99));

    let (store, fail_apply, _fail_set_peak) = FaultStore::new(common::new_store().await);
    let mut chaser = Chaser::new(Engine::new(store, NativePrimitives, MAINNET), cfg());
    let mut staged = chaser
        .stage_window_pre(chain, None)
        .await
        .expect("window stages");
    let constants = MAINNET;
    let verdict = drain_staged_window(&NativePrimitives, &constants, staged.take_drain_input());

    fail_apply.store(true, Ordering::Relaxed);
    chaser
        .confirm_window_pre(staged, verdict)
        .await
        .expect_err("the injected store fault surfaces");
    let (_, _, overlay) = chaser.engine().collection_sizes();
    assert_eq!(
        overlay, 0,
        "a confirm that failed in the store left {overlay} staged overlay entries resident"
    );
}
