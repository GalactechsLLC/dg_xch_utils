// Adversarial-peer / backpressure invariants (test class 4): with ONE misbehaving peer and ONE
// good peer, the sync pipeline keeps forward progress within a hard bound for EVERY misbehavior
// mode — no wedge, no starvation. Modes: never-reply (the BlockRangeSource analog of the
// never-draining sink in core's `send_timeout_tests`), reject-all-ranges, stale-peak (honest but
// behind), disconnect-mid-window. The misbehaving peer's reservations must be reclaimed (t051's
// stall-reclaim generalized across modes), its failure budget must retire it, and the good peer
// must finish the range; the server-shaped follow rotation must confirm the peak within a fixed
// tick budget.

mod common;

use common::{
    FixtureSource, MemSource, MisbehavingSource, Misbehavior, candidate_record, seed_record_for,
};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::{Chaser, Engine, NativePrimitives, SyncConfig};
use dg_xch_stores::{BlockStore, SqliteStore};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const BASE: u32 = 6_000_000;
const N: u32 = 48;

fn cfg() -> SyncConfig {
    SyncConfig {
        peers: 2,
        window: 32,
        batch: 8,
        request_timeout: Duration::from_millis(300),
        assume_valid: 0,
    }
}

// The confirm-path tests use the assume-valid milestone above the fixture block: the subject
// here is the DRIVER's resilience beside a misbehaving peer, not PoW/VDF cost — full validation of
// this exact block is pinned by t053/t056, and a 20s+ verify on a loaded builder would turn every
// wall-clock wedge bound into a flake.
fn confirm_cfg() -> SyncConfig {
    SyncConfig {
        assume_valid: 5_000_001,
        ..cfg()
    }
}

// Every misbehavior mode, named for the per-mode assertion messages.
fn all_modes(stale_tip: u32) -> [(&'static str, Misbehavior); 4] {
    [
        ("never-reply", Misbehavior::NeverReply),
        ("reject-all-ranges", Misbehavior::RejectAllRanges),
        ("stale-peak", Misbehavior::StalePeak { tip: stale_tip }),
        ("disconnect-mid-window", Misbehavior::DisconnectMidWindow),
    ]
}

async fn seed_candidates(store: &SqliteStore, base_block: &FullBlock, n: u32) {
    let template = common::load_records()[0].clone();
    let records: Vec<_> = (BASE..BASE + n)
        .map(|h| candidate_record(&template, base_block, h))
        .collect();
    store
        .add_block_records(&records)
        .await
        .expect("seed records");
}

// Body-pipeline forward progress: the 48-body range drains within a hard wall bound beside every
// misbehaving peer — the reservation window reclaims what the bad peer holds, the failure budget
// retires it, and the good peer finishes the whole range (no gap, no hang).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn body_pipeline_drains_beside_every_misbehaving_peer() {
    let base_block = common::load_full_block(5_000_000);
    for (name, mode) in all_modes(BASE + 15) {
        let store = common::new_store().await;
        seed_candidates(&store, &base_block, N).await;
        let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
        let mut chaser = Chaser::new(engine, cfg());
        let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
            Arc::new(MisbehavingSource::new(99, mode, base_block.clone())),
            Arc::new(MemSource {
                id: 1,
                base: base_block.clone(),
            }),
        ];
        tokio::time::timeout(Duration::from_secs(30), chaser.sync_bodies(&sources))
            .await
            .unwrap_or_else(|_| panic!("{name}: the pipeline wedged (no completion in the bound)"))
            .unwrap_or_else(|e| panic!("{name}: sync_bodies must finish beside the bad peer: {e}"));
        let leftover = chaser
            .engine()
            .store()
            .get_unassociated(10)
            .await
            .expect("unassociated");
        assert!(
            leftover.is_empty(),
            "{name}: every candidate body written through — no gap from the bad peer"
        );
        assert!(
            chaser.metrics().blocks_downloaded.load(Ordering::Relaxed) >= u64::from(N),
            "{name}: the full range was downloaded"
        );
        if matches!(mode, Misbehavior::NeverReply) {
            assert!(
                chaser.metrics().reclaimed.load(Ordering::Relaxed) >= 1,
                "{name}: the timed-out reservation was reclaimed for the good peer"
            );
        }
    }
}

// Confirmed-peak forward progress through the download+confirm seam (the t056 driver seam) beside
// every misbehaving peer: from an empty store, the confirmed peak must still advance to the served
// real block. Disconnect-mid-window is honest-then-drop (its one serve is the real body), so
// progress may come from either peer — what must never happen is a wedge or a missing peak.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirmed_peak_advances_beside_every_misbehaving_peer() {
    let block = common::load_full_block(5_000_000);
    for (name, mode) in all_modes(4_999_000) {
        let store = common::new_store().await;
        let template = common::load_records()[0].clone();
        store
            .add_block_records(&[seed_record_for(&template, &block)])
            .await
            .expect("seed candidate");
        let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
        let mut chaser = Chaser::new(engine, confirm_cfg());
        let mut fixture = HashMap::new();
        fixture.insert(5_000_000u32, block.clone());
        let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
            Arc::new(MisbehavingSource::new(99, mode, block.clone()).with_serve(fixture.clone())),
            Arc::new(FixtureSource {
                id: 1,
                blocks: fixture,
            }),
        ];
        let peak = tokio::time::timeout(Duration::from_secs(60), chaser.sync_range(&sources))
            .await
            .unwrap_or_else(|_| panic!("{name}: sync_range wedged"))
            .unwrap_or_else(|e| panic!("{name}: sync_range must confirm beside the bad peer: {e}"));
        assert_eq!(
            peak,
            Some((block.header_hash().unwrap(), 5_000_000)),
            "{name}: the confirmed peak advanced to the served block"
        );
    }
}

// The server-shaped follow tick: rotate to the next peer each tick (the registry.live_peers +
// follow_rotation pattern — closed peers are skipped, a failed or timed-out tick rotates), each
// tick bounded like the driver's request timeout. The confirmed peak must land within a fixed tick
// budget for every mode — a misbehaving peer costs at most its own tick, never a wedge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn follow_tick_rotation_confirms_the_peak_within_a_bounded_tick_budget() {
    let block = common::load_full_block(5_000_000);
    for (name, mode) in all_modes(4_999_000) {
        let store = common::new_store().await;
        let engine = Engine::new(Arc::new(store), NativePrimitives, MAINNET);
        let mut chaser = Chaser::new(engine, confirm_cfg());
        let mut fixture = HashMap::new();
        fixture.insert(5_000_000u32, block.clone());
        let sources: Vec<Arc<dyn BlockRangeSource>> = vec![
            Arc::new(MisbehavingSource::new(99, mode, block.clone()).with_serve(fixture.clone())),
            Arc::new(FixtureSource {
                id: 1,
                blocks: fixture,
            }),
        ];
        let mut peak = None;
        let mut ticks = 0usize;
        while peak.is_none() {
            assert!(
                ticks < 4,
                "{name}: no confirmed-peak progress within the tick budget"
            );
            let source = &sources[ticks % sources.len()];
            ticks += 1;
            if source.is_closed() {
                continue; // the driver only follows live peers
            }
            // A failed or timed-out tick leaves `peak` unset and rotates to the next peer (the
            // never-reply peer burns exactly one bounded tick, never a wedge).
            if let Ok(Ok(p)) = tokio::time::timeout(
                Duration::from_secs(10),
                chaser.follow_to(source, 5_000_000, 5_000_000),
            )
            .await
            {
                peak = p;
            }
        }
        assert_eq!(
            peak,
            Some((block.header_hash().unwrap(), 5_000_000)),
            "{name}: the follow confirmed the peak"
        );
        assert!(ticks <= 3, "{name}: converged in {ticks} ticks");
    }
}
