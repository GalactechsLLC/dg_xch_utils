// The mempool's fee-estimator event hooks: admission feeds `add_mempool_item`,
// and a new peak that includes a resident item feeds the confirmation signal (`new_block` /
// `process_block`). Proves the wiring the audit flagged as missing — a real Mempool driven end to
// end, asserting the tracker state moves. Companion to the tracker-level unit tests in
// node/src/fee_estimator.rs (bucketing/decay/convergence).

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend::Spend;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::mempool::Mempool;
use dg_xch_stores::{CoinStore, SqliteStore};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn store() -> SqliteStore {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "dg_xch_feeest_hooks_{}_{n}.sqlite",
        std::process::id()
    ));
    SqliteStore::open(&path).await.expect("open store")
}

fn h(tag: u8) -> Bytes32 {
    Bytes32::from([tag; 32])
}

fn coin(tag: u8, amount: u64) -> Coin {
    Coin {
        parent_coin_info: h(tag),
        puzzle_hash: h(tag ^ 0xff),
        amount,
    }
}

fn record(c: Coin, height: u32) -> CoinRecord {
    CoinRecord {
        coin: c,
        confirmed_block_index: height,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

fn mk_spend(c: &Coin) -> Spend {
    Spend {
        parent_id: c.parent_coin_info,
        coin_amount: c.amount,
        puzzle_hash: c.puzzle_hash,
        coin_id: c.name(),
        height_relative: None,
        seconds_relative: None,
        before_height_relative: None,
        before_seconds_relative: None,
        birth_height: None,
        birth_seconds: None,
        create_coin: HashSet::new(),
        agg_sig_me: vec![],
        agg_sig_parent: vec![],
        agg_sig_puzzle: vec![],
        agg_sig_amount: vec![],
        agg_sig_puzzle_amount: vec![],
        agg_sig_parent_amount: vec![],
        agg_sig_parent_puzzle: vec![],
        create_coin_announcements: vec![],
        assert_coin_announcements: vec![],
        create_puzzle_announcements: vec![],
        assert_puzzle_announcements: vec![],
        assert_concurrent_spend: vec![],
        assert_concurrent_puzzle: vec![],
        assert_ephemeral: false,
        sent_messages: vec![],
        received_messages: vec![],
        flags: 0,
        condition_cost: 0,
        execution_cost: 0,
    }
}

fn conds(spends: Vec<Spend>, fee: u64, cost: u64) -> SpendBundleConditions {
    let removal: u128 = spends.iter().map(|s| u128::from(s.coin_amount)).sum();
    SpendBundleConditions {
        spends,
        reserve_fee: fee,
        height_absolute: 0,
        seconds_absolute: 0,
        before_height_absolute: None,
        before_seconds_absolute: None,
        agg_sig_unsafe: vec![],
        falcon_signatures: vec![],
        cost,
        removal_amount: removal,
        addition_amount: removal - u128::from(fee),
    }
}

fn bundle(sig_tag: u8) -> SpendBundle {
    SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([sig_tag; 96]),
    }
}

// Admission feeds add_mempool_item; a new peak that includes the item feeds the confirmation
// signal. The tracker's outstanding count, latest-seen height, and first-recorded height are the
// observables.
#[tokio::test]
async fn admission_and_block_inclusion_feed_the_tracker() {
    let store = store().await;
    store
        .apply_block(100, 0, &[record(coin(1, 1_000), 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // Baseline: the estimator has seen nothing.
    assert_eq!(mp.fee_estimator().tracker().latest_seen_height(), 0);
    assert_eq!(mp.fee_estimator().tracker().first_recorded_height(), 0);
    assert_eq!(mp.fee_estimator().tracker().outstanding_short(), 0);

    // Admit → add_mempool_item fires: the item joins the unconfirmed population.
    mp.admit(
        &store,
        bundle(0xa1),
        conds(vec![mk_spend(&coin(1, 1_000))], 100, 1_000),
    )
    .await
    .expect("admitted");
    assert_eq!(
        mp.fee_estimator().tracker().outstanding_short(),
        1,
        "add_mempool_item hook must feed the tracker on admission"
    );
    assert_eq!(mp.len(), 1);

    // A new peak two blocks later that spends the coin — the item is block-included, so the
    // estimator records a confirmation (process_block advances latest/first-recorded height).
    let spent = [coin(1, 1_000).name()];
    mp.new_peak(&store, 102, 0, &spent).await.expect("new peak");
    assert_eq!(mp.len(), 0, "block-included item leaves the mempool");
    assert_eq!(
        mp.fee_estimator().tracker().latest_seen_height(),
        102,
        "new_block hook must advance latest_seen_height"
    );
    assert_eq!(
        mp.fee_estimator().tracker().first_recorded_height(),
        102,
        "a block that confirmed a tracked item sets first_recorded_height"
    );
}

// An empty (never-fed) estimator quotes the floor rate of 0 for every horizon.
#[tokio::test]
async fn fresh_mempool_estimates_floor_zero() {
    let mp = Mempool::new(&MAINNET);
    for t in [0u64, 60, 600, 3600] {
        assert_eq!(
            mp.fee_estimator().estimate_fee_rate(t),
            0.0,
            "empty mempool → floor 0 at t={t}"
        );
    }
}

// Sustained confirmed pressure, driven through the mempool's own estimator handle, converges on a
// positive fee-rate — and higher fee-per-cost yields a strictly higher estimate. Uses the block
// ingestion API directly to keep the harness bounded.
#[tokio::test]
async fn steady_pressure_through_mempool_estimator_is_positive_and_ordered() {
    let mut low = Mempool::new(&MAINNET);
    let mut high = Mempool::new(&MAINNET);
    // wait = 1: txs confirm the next block, so even the shortest target has data.
    for height in 100u32..300 {
        low.fee_estimator_mut().ingest_block(
            height,
            &[(5_000_000, 10_000_000, height - 1)],
            5_000_000,
        ); // fpc 2
        high.fee_estimator_mut().ingest_block(
            height,
            &[(5_000_000, 100_000_000, height - 1)],
            5_000_000,
        ); // fpc 20
    }
    let low_rate = low.fee_estimator().estimate_fee_rate(0);
    let high_rate = high.fee_estimator().estimate_fee_rate(0);
    assert!(
        low_rate > 0.0,
        "sustained pressure must produce a positive estimate, got {low_rate}"
    );
    assert!(
        high_rate > low_rate,
        "higher fee-per-cost → higher estimate: low={low_rate} high={high_rate}"
    );
}
