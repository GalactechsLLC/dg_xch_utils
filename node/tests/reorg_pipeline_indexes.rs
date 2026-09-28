//! Reorg-path index build — a reorg driven through the WINDOW-CONFIRM pipeline
//! (`Engine::confirm_staged_batch`, the entry the server's `follow_blocks_reporting` uses) while
//! BEHIND tip must complete, and must build the reorg-speed coin indexes it needs.
//!
//! The failing case: with the store-backed fork reconstruction in place, a 1-block equal-weight
//! reorg can INITIATE below tip, but it then
//! hangs — `follow_inflight` grows forever, CPU idle, zero commits. Root cause: the reorg's rollback
//! (`rollback_to`: `DELETE WHERE confirmed_index > $1` / `UPDATE WHERE spent_index > $1`) and its
//! rolled-back-state read (`rolled_back_coin_states`: per-height `confirmed_index`/`spent_index`
//! lookups) filter `coin_record` by columns whose indexes the SQL backends DEFER to
//! `build_indexes` at the sync->tip transition. A node stuck on a minority tie-break branch reorgs
//! BELOW tip — long before that build fires — so on a large coin table each reorg query
//! seq-scans the whole table, stalling the reorg indefinitely.
//!
//! Fix: `CoinStore::ensure_reorg_indexes`, called at the top of the engine's `reorg`, builds those
//! indexes idempotently before any rollback query runs, so the rollback works at any sync depth.
//! This test proves, on the pipeline confirm path: the
//! reorg-tier indexes are ABSENT at open (deferred), a reorg driven through `confirm_staged_batch`
//! below tip COMPLETES, and those indexes are PRESENT afterward.

mod common;

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::engine::BlockDelta;
use dg_xch_node::{AddBlockOutcome, Engine, NativePrimitives};
use dg_xch_stores::{BlockStore, SqliteStore};
use sqlx::ConnectOptions;
use sqlx::sqlite::SqliteConnectOptions;
use std::path::Path;

const H: u32 = 6_000_100; // the tie height; P is H-1

fn record(
    template: &BlockRecord,
    tag: u8,
    height: u32,
    weight: u128,
    prev: Bytes32,
) -> BlockRecord {
    let mut r = template.clone();
    r.header_hash = common::synth_hash(tag, height);
    r.prev_hash = prev;
    r.height = height;
    r.weight = weight;
    r.total_iters = weight;
    r.timestamp = Some(1_700_000_000 + u64::from(height));
    r.sub_epoch_summary_included = None;
    r
}

fn delta(r: &BlockRecord, additions: Vec<CoinRecord>) -> BlockDelta {
    BlockDelta {
        header_hash: r.header_hash,
        prev_hash: r.prev_hash,
        height: r.height,
        weight: r.weight,
        timestamp: r.timestamp.unwrap_or(0),
        record: r.clone(),
        additions,
        removals: Vec::new(),
        hints: Vec::new(),
    }
}

fn coin_at(tag: u8, height: u32) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: common::synth_hash(tag, height),
            puzzle_hash: common::synth_hash(tag ^ 0xff, height),
            amount: 1_000,
        },
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 1_700_000_000 + u64::from(height),
        spent: false,
    }
}

/// Whether a named index exists on `coin_record` in the SQLite file (a fresh read-only connection,
/// so it reflects only committed DDL — exactly what a restarted node would see).
async fn index_exists(path: &Path, name: &str) -> bool {
    let mut conn = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .connect()
        .await
        .expect("open sqlite for index check");
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE type='index' AND tbl_name='coin_record' AND name=?",
    )
    .bind(name)
    .fetch_one(&mut conn)
    .await
    .expect("query sqlite_master");
    n > 0
}

// A reorg driven through the window-confirm pipeline BELOW tip completes and builds the deferred
// reorg indexes:
//
//   P(H-1) ── A(H, weight W)          [confirmed main chain, the peak]
//          └─ B(H, weight W == A)     [orphan]
//               └─ C(H+1, weight > W) [heavier — fed through confirm_staged_batch]
//
// P and A create coins (so the rollback/rolled-back-state queries have rows to touch); the peak is
// A@H — stale relative to any live tip, so this is the below-tip, pre-`build_indexes` regime.
#[tokio::test]
async fn reorg_through_confirm_pipeline_builds_reorg_indexes_below_tip() {
    let path = common::unique_db_path();
    let template = common::load_records()[0].clone();

    let p = record(
        &template,
        0xf0,
        H - 1,
        9_010,
        common::synth_hash(0xf1, H - 2),
    );
    let a = record(&template, 0xa0, H, 9_100, p.header_hash);
    let b = record(&template, 0xb0, H, 9_100, p.header_hash); // equal weight ⇒ orphan
    let c = record(&template, 0xc0, H + 1, 9_200, b.header_hash); // heavier ⇒ reorg

    let store = SqliteStore::open(&path).await.expect("open");
    let mut engine = Engine::new(store, NativePrimitives, MAINNET).with_enforced_coin_rules();

    // Base P (creates coin_p), main chain A (creates coin_a, the peak), orphan B.
    assert_eq!(
        engine
            .add_delta(delta(&p, vec![coin_at(0x11, H - 1)]))
            .await
            .unwrap(),
        AddBlockOutcome::NewPeak { height: H - 1 }
    );
    assert_eq!(
        engine
            .add_delta(delta(&a, vec![coin_at(0x22, H)]))
            .await
            .unwrap(),
        AddBlockOutcome::Extended { height: H }
    );
    assert_eq!(
        engine.add_delta(delta(&b, Vec::new())).await.unwrap(),
        AddBlockOutcome::Orphan { height: H }
    );

    // The reorg-tier indexes are DEFERRED (never created at open) — the below-tip regime this
    // test exercises, where a rollback query would otherwise seq-scan.
    assert!(
        !index_exists(&path, "coin_record_confirmed_index").await,
        "reorg indexes must be deferred (absent) before the reorg"
    );
    assert!(!index_exists(&path, "coin_record_spent_index").await);

    // C's record is persisted by the staging phase before confirm in the real pipeline
    // (`stage_block_pre` -> `persist_archive`); replicate that precondition here so
    // `confirm_staged_batch` (which confirms already-staged records) can flip the peak to it.
    engine
        .store()
        .add_block_records(std::slice::from_ref(&c))
        .await
        .expect("persist C's staged record");

    // Drive the heavier branch through the WINDOW-CONFIRM path (not add_block / add_delta): this is
    // the entry `follow_blocks_reporting` calls. C does not extend the peak A, so it takes the
    // per-delta fork-choice path → reorg.
    let outcomes = engine
        .confirm_staged_batch(vec![delta(&c, vec![coin_at(0x33, H + 1)])])
        .await
        .expect("the pipeline reorg must complete, not seq-scan-hang");
    assert_eq!(
        outcomes,
        vec![AddBlockOutcome::Reorg {
            fork_height: H - 1,
            links: 2
        }],
        "confirm_staged_batch reorgs to the heavier branch [B, C], forking at P (H-1)"
    );
    assert_eq!(
        engine.store().get_peak().await.unwrap(),
        Some((c.header_hash, H + 1)),
        "the pipeline reorg advanced the peak to the heavier tip C"
    );

    // The reorg built the indexes its rollback queries need — a below-tip reorg no longer depends
    // on the sync->tip `build_indexes` ever firing.
    assert!(
        index_exists(&path, "coin_record_confirmed_index").await,
        "the reorg must have built coin_record_confirmed_index"
    );
    assert!(
        index_exists(&path, "coin_record_spent_index").await,
        "the reorg must have built coin_record_spent_index"
    );
}
