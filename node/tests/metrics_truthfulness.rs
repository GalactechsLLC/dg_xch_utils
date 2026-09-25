// Instrumentation truthfulness. The retention hunt and the instrumentation gap-closing
// (f7414aa "close the measured instrumentation gaps") both turned on one
// question: CAN THE GAUGES BE BELIEVED? A counter that moves without its phenomenon sends the
// on-call down a false trail; one that fails to move hides the real one. Nothing owed either
// direction as a test. This file pins the truth table for the SyncMetrics surface at the chaser
// seam: every signal starts at zero, moves when — and only when — its phenomenon occurs, and the
// fault signals stay silent on the clean path (the half no incidental assertion elsewhere covers).

mod common;

use common::{MemSource, StallingSource};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::BlockStore;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE: u32 = 6_000_000;
const N: u32 = 32;

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 2,
        window: 32,
        batch: 8,
        request_timeout: Duration::from_millis(300),
        assume_valid: 5_000_001,
    }
}

async fn seeded_chaser(
    n: u32,
    config: SyncConfig,
) -> Chaser<Arc<dg_xch_stores::SqliteStore>, NativePrimitives> {
    let base_block = common::load_full_block(5_000_000);
    let template = common::load_records()[0].clone();
    let store = common::new_store().await;
    let records: Vec<_> = (BASE..BASE + n)
        .map(|h| common::candidate_record(&template, &base_block, h))
        .collect();
    store.add_block_records(&records).await.expect("seed");
    Chaser::new(
        Engine::new(Arc::new(store), NativePrimitives, MAINNET),
        config,
    )
}

// Zero state: a fresh chaser reports NO signal — no phantom downloads, confirms, reclaims, or
// reorgs before anything happened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_chaser_reports_zero_signals() {
    let chaser = seeded_chaser(0, cfg()).await;
    let m = chaser.metrics();
    assert_eq!(m.blocks_downloaded.load(Ordering::Relaxed), 0);
    assert_eq!(m.blocks_confirmed.load(Ordering::Relaxed), 0);
    assert_eq!(m.reclaimed.load(Ordering::Relaxed), 0);
    assert_eq!(m.peak_window.load(Ordering::Relaxed), 0);
    assert_eq!(m.peak_inflight_blocks.load(Ordering::Relaxed), 0);
    assert_eq!(m.last_reorg_depth.load(Ordering::Relaxed), 0);
    assert_eq!(m.window_blocks.load(Ordering::Relaxed), 0);
}

// Clean path: a healthy 2-peer download moves the work signals inside their proven bounds — and
// the FAULT signals stay at zero. `reclaimed == 0` on a clean sync is the truth-table half t051
// never pinned: a reclaim counter that ticks without a stall is a false incident. The request
// timeout is generous here so a CPU-starved (but instant) in-memory fetch cannot masquerade as a
// stall on a loaded builder — the phenomenon under test is the peer's behavior, not the box's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clean_sync_moves_work_signals_and_keeps_fault_signals_silent() {
    let base_block = common::load_full_block(5_000_000);
    let mut chaser = seeded_chaser(
        N,
        SyncConfig {
            request_timeout: Duration::from_secs(10),
            ..cfg()
        },
    )
    .await;
    let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
        Arc::new(MemSource {
            id: 1,
            base: base_block.clone(),
        }),
        Arc::new(MemSource {
            id: 2,
            base: base_block,
        }),
    ];
    chaser.sync_bodies(&sources).await.expect("clean sync");
    let m = chaser.metrics();

    // Work signals: moved, inside the architecture's hard bounds.
    let downloaded = m.blocks_downloaded.load(Ordering::Relaxed);
    assert!(downloaded >= u64::from(N), "every body counted");
    let window = m.peak_window.load(Ordering::Relaxed);
    assert!(
        window > 0 && window <= cfg().window,
        "window peaked in (0, cap]"
    );
    let inflight = m.peak_inflight_blocks.load(Ordering::Relaxed);
    let inflight_cap = cfg().peers * cfg().batch as usize;
    assert!(
        inflight > 0 && inflight <= inflight_cap,
        "in-flight peaked in (0, P*batch]"
    );

    // Fault signals: SILENT. No stall happened, so no reclaim may be reported; download-only
    // moves no confirm and no reorg.
    assert_eq!(
        m.reclaimed.load(Ordering::Relaxed),
        0,
        "a clean sync must not report reclaims (false incident)"
    );
    assert_eq!(m.blocks_confirmed.load(Ordering::Relaxed), 0);
    assert_eq!(m.last_reorg_depth.load(Ordering::Relaxed), 0);
}

// Fault path: the same range beside a stalled peer moves the fault signal — the pairing with the
// clean-path zero above is what makes `reclaimed` a believable incident counter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stalled_peer_moves_the_reclaim_signal() {
    let base_block = common::load_full_block(5_000_000);
    let mut chaser = seeded_chaser(N, cfg()).await;
    let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
        Arc::new(StallingSource { id: 99 }),
        Arc::new(MemSource {
            id: 1,
            base: base_block,
        }),
    ];
    chaser
        .sync_bodies(&sources)
        .await
        .expect("sync beside stall");
    assert!(
        chaser.metrics().reclaimed.load(Ordering::Relaxed) >= 1,
        "a stalled reservation must be reported as reclaimed"
    );
}

// Confirm path: following one block moves exactly the confirm-side signals — one confirmed block,
// a one-block window, no phantom reorg — and a second look at the same block (AlreadyHave) moves
// the confirm counter no further while the engine gauges now report the retained record (the
// retention-bisect signal must see what the engine holds).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirm_signals_track_the_follow_window_exactly() {
    let block = common::load_full_block(5_000_000);
    let store = common::new_store().await;
    common::ancestry::seed_mainnet_parent(&store).await;
    let mut chaser = Chaser::new(
        Engine::new(Arc::new(store), NativePrimitives, MAINNET),
        cfg(),
    );

    let peak = chaser
        .follow_blocks(std::slice::from_ref(&block))
        .await
        .expect("confirm");
    assert_eq!(peak, Some((block.header_hash().unwrap(), 5_000_000)));
    let m = chaser.metrics().clone();
    assert_eq!(
        m.blocks_confirmed.load(Ordering::Relaxed),
        1,
        "exactly one confirm reported"
    );
    assert_eq!(
        m.window_blocks.load(Ordering::Relaxed),
        1,
        "the window composition reports the one-block window"
    );
    assert_eq!(
        m.last_reorg_depth.load(Ordering::Relaxed),
        0,
        "no phantom reorg on a plain extension"
    );

    // Second window over the same block: AlreadyHave — the confirm counter must NOT move again,
    // and the engine gauges (sampled at window entry) now see the retained record.
    chaser
        .follow_blocks(std::slice::from_ref(&block))
        .await
        .expect("already-have follow");
    assert_eq!(
        m.blocks_confirmed.load(Ordering::Relaxed),
        1,
        "AlreadyHave must not inflate the confirm counter"
    );
    assert!(
        m.engine_cache_records.load(Ordering::Relaxed) >= 1,
        "the engine gauge reports the retained record"
    );

    // The confirmed peak the metrics describe is the peak the store holds.
    assert_eq!(
        chaser.engine().store().get_peak().await.expect("get_peak"),
        Some((block.header_hash().unwrap(), 5_000_000)),
        "metrics and store agree on the confirmed peak"
    );
}
