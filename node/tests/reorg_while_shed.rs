// Reorg WHILE the service indexes are SHED — the idxphase falling edge meets the reorg path.
// During a deep re-catch-up the server sheds every secondary `coin_record` index
// (`full-node/src/node/runtime.rs` `update_synced` falling-edge latch, pinned by
// `deep_fall_behind_sheds_indexes_once_and_the_tip_edge_rebuilds`); reorgs are a tip
// phenomenon, but a node CAN be asked to reorg while shed — a minority-branch node must rejoin
// the heavier chain mid-catch-up, long before the rising-edge `build_indexes` fires. The
// contract (stores/src/traits.rs `shed_service_indexes` / `ensure_reorg_indexes` docs): the
// engine's reorg rebuilds the reorg tier ON DEMAND before the rollback runs — slower, correct —
// and it must complete beside a live archive writer (the bulk download's write-through keeps
// committing record batches while the DDL and the single reorg transaction run), with no
// writer-lock deadlock. One suite, both SQL backends:
//
//   - shed drops the reorg tier (`spent_index` on both backends; `confirmed_index` too on
//     SQLite, where it is a full btree) — probed against the live catalog,
//   - the reorg lands INSIDE a hard wall bound beside a concurrent archive-writer task,
//   - the flip is exact (the lineage replay-equality invariant from common/lineage.rs),
//   - afterward BOTH reorg indexes exist again (rebuilt on demand) while the service tier
//     (`puzzle_hash`) stays shed — the rising-edge `build_indexes` owns that rebuild, not the
//     reorg.

mod common;

use common::lineage::{FORK, assert_equals_replay, build_branches, touched_names};
use common::synth_hash;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::engine::BlockDelta;
use dg_xch_node::{AddBlockOutcome, Engine, NativePrimitives};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const N: u32 = 30;
// The archive writer commits candidate-record batches at far-away heights — the write-through
// downloader's shape (records/bodies, never coins: coins are applied only by the engine's
// confirm) — so it contends for the writer lock/connection without touching the coin state the
// reorg asserts over.
const WRITER_BASE: u32 = 9_000_000;

async fn archive_writer<S: BlockStore + CoinStore + Send + Sync>(
    store: Arc<S>,
    stop: Arc<AtomicBool>,
) -> u64 {
    let template = common::load_records()[0].clone();
    let mut batches = 0u64;
    let mut i = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let records: Vec<_> = (0..8u32)
            .map(|k| {
                let h = WRITER_BASE + i * 8 + k;
                let mut r = template.clone();
                r.header_hash = synth_hash(0xee, h);
                r.prev_hash = synth_hash(0xee, h.wrapping_sub(1));
                r.height = h;
                r.weight = u128::from(h);
                r.total_iters = u128::from(h);
                r.sub_epoch_summary_included = None;
                r
            })
            .collect();
        let mut batch = store.begin().await.expect("writer begin");
        store
            .add_block_records_in(&mut batch, &records)
            .await
            .expect("writer records");
        store.commit(batch).await.expect("writer commit");
        batches += 1;
        i += 1;
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    batches
}

// The generic core: build the two branches, confirm A, park B, SHED, then land the flip beside
// the live writer inside a hard bound. Returns after asserting the exact flip.
async fn run_reorg_while_shed<S>(store: Arc<S>)
where
    S: BlockStore + CoinStore + Send + Sync + 'static,
{
    let br = build_branches(N);
    let mut engine = Engine::new(store.clone(), NativePrimitives, MAINNET);
    engine.add_delta(br.base.clone()).await.unwrap();
    for d in &br.a {
        engine.add_delta(d.clone()).await.unwrap();
    }
    for d in &br.b[..N as usize] {
        assert_eq!(
            engine.add_delta(d.clone()).await.unwrap(),
            AddBlockOutcome::Orphan { height: d.height }
        );
    }

    // The falling edge: what the server's deep-catch-up latch fires.
    store
        .shed_service_indexes()
        .await
        .expect("shed the service tier");

    // A live archive writer beside the reorg — the bulk download's write-through.
    let stop = Arc::new(AtomicBool::new(false));
    let writer = tokio::spawn(archive_writer(store.clone(), stop.clone()));

    // The flip must land within a hard wall bound: ensure_reorg_indexes' on-demand DDL, the
    // single reorg transaction, and the writer's batches interleave — never deadlock.
    let b_tip = br.b.last().unwrap().clone();
    let outcome = tokio::time::timeout(Duration::from_secs(60), engine.add_delta(b_tip.clone()))
        .await
        .expect("the shed reorg must not deadlock against the archive writer")
        .expect("the shed reorg lands");
    match outcome {
        AddBlockOutcome::Reorg { fork_height, .. } => assert_eq!(fork_height, FORK),
        other => panic!("expected the reorg, got {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    let batches = writer.await.expect("writer task joins");
    assert!(
        batches > 0,
        "the archive writer made progress beside the reorg"
    );

    let names = touched_names(N);
    let chain: Vec<&BlockDelta> = std::iter::once(&br.base).chain(br.b.iter()).collect();
    assert_equals_replay(
        store.as_ref(),
        (b_tip.header_hash, FORK + N + 1),
        &chain,
        &names,
        "after the reorg-while-shed flip",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_reorg_while_shed_rebuilds_the_reorg_tier_and_lands_beside_a_writer() {
    let path = common::unique_db_path();
    let store = Arc::new(SqliteStore::open(&path).await.expect("open"));

    // Reach the "was at tip" posture: the full index set built (the rising edge)…
    store.build_indexes().await.expect("build indexes at tip");
    // …then run the shed + reorg + writer core.
    run_reorg_while_shed(store).await;

    // Catalog probe: both reorg indexes rebuilt on demand; the service tier stays shed.
    let mut conn = <sqlx::sqlite::SqliteConnection as sqlx::Connection>::connect(&format!(
        "sqlite://{}",
        path.display()
    ))
    .await
    .expect("probe connection");
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'coin_record'",
    )
    .fetch_all(&mut conn)
    .await
    .expect("index catalog");
    let names: Vec<&str> = rows.iter().map(|(n,)| n.as_str()).collect();
    assert!(
        names.contains(&"coin_record_confirmed_index"),
        "the reorg rebuilt the confirmed_index btree on demand, got {names:?}"
    );
    assert!(
        names.contains(&"coin_record_spent_index"),
        "the reorg rebuilt the spent_index btree on demand, got {names:?}"
    );
    assert!(
        !names.contains(&"coin_record_puzzle_hash"),
        "the service tier stays shed — its rebuild belongs to the rising edge, got {names:?}"
    );
}
