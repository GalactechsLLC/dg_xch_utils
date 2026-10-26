use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_core::blockchain::spend::{NewCoin, Spend};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_core::consensus::block_generator::{
    CoinSpendContext, ConditionValidationContext, validate_block_conditions,
};
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_node::mempool::{Mempool, MempoolError};
use dg_xch_stores::{CoinStore, SqliteStore};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

async fn store() -> SqliteStore {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("dg_xch_mempool_{}_{n}.sqlite", std::process::id()));
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

// A distinct aggregated_signature per bundle so bundle.name() differs (the mempool key).
fn bundle(sig_tag: u8) -> SpendBundle {
    SpendBundle {
        authorization: Default::default(),
        coin_spends: vec![],
        aggregated_signature: Bytes96::from([sig_tag; 96]),
    }
}

#[tokio::test]
async fn valid_bundle_admitted_in_fee_order() {
    let store = store().await;
    store
        .apply_block(
            100,
            0,
            &[record(coin(1, 1_000), 100), record(coin(2, 1_000), 100)],
            &[],
        )
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // low fee-per-cost (0.1) then high (0.5); fee order must return the high one first
    let low = mp
        .admit(
            &store,
            bundle(0xa1),
            conds(vec![mk_spend(&coin(1, 1_000))], 100, 1_000),
        )
        .await
        .expect("low admitted");
    let high = mp
        .admit(
            &store,
            bundle(0xa2),
            conds(vec![mk_spend(&coin(2, 1_000))], 500, 1_000),
        )
        .await
        .expect("high admitted");

    assert_eq!(mp.len(), 2);
    let ordered = mp.items_by_fee();
    assert_eq!(ordered[0].name, high, "highest fee-per-cost first");
    assert_eq!(ordered[1].name, low);
    assert!(ordered[0].fee_per_cost() > ordered[1].fee_per_cost());
}

#[tokio::test]
async fn double_spend_of_confirmed_coin_rejected_with_reason() {
    let store = store().await;
    // coin created at 100, spent on-chain at 101 -> already spent at peak
    store
        .apply_block(100, 0, &[record(coin(3, 500), 100)], &[])
        .await
        .unwrap();
    store
        .apply_block(101, 0, &[], &[coin(3, 500).name()])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(101, 0);

    let err = mp
        .admit(
            &store,
            bundle(0xb1),
            conds(vec![mk_spend(&coin(3, 500))], 50, 1_000),
        )
        .await
        .expect_err("double spend must be rejected");
    match err {
        MempoolError::DoubleSpend(id) => assert_eq!(id, coin(3, 500).name()),
        other => panic!("expected DoubleSpend, got {other:?}"),
    }
}

#[tokio::test]
async fn conflicting_bundle_below_min_fee_increase_rejected() {
    let store = store().await;
    store
        .apply_block(100, 0, &[record(coin(4, 1_000), 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    mp.admit(
        &store,
        bundle(0xc1),
        conds(vec![mk_spend(&coin(4, 1_000))], 100, 1_000),
    )
    .await
    .expect("first admitted");
    // A 100-mojo bump is far under MEMPOOL_MIN_FEE_INCREASE (0.00001 XCH), so replace-by-fee
    // does not apply and the conflict is rejected.
    let err = mp
        .admit(
            &store,
            bundle(0xc2),
            conds(vec![mk_spend(&coin(4, 1_000))], 200, 1_000),
        )
        .await
        .expect_err("conflicting spend below the fee-bump floor must be rejected");
    match err {
        MempoolError::Conflict(id) => assert_eq!(id, coin(4, 1_000).name()),
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(mp.len(), 1);
}

#[tokio::test]
async fn unknown_unspent_rejected() {
    let store = store().await;
    store
        .apply_block(100, 0, &[record(coin(5, 1_000), 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);
    // spend a coin that never existed on-chain and isn't created in-bundle
    let err = mp
        .admit(
            &store,
            bundle(0xd1),
            conds(vec![mk_spend(&coin(9, 1_000))], 100, 1_000),
        )
        .await
        .expect_err("unknown coin must be rejected");
    assert!(
        matches!(err, MempoolError::UnknownUnspent(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn item_dropped_when_its_coin_is_spent_by_new_peak() {
    let store = store().await;
    store
        .apply_block(100, 0, &[record(coin(6, 1_000), 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);
    mp.admit(
        &store,
        bundle(0xe1),
        conds(vec![mk_spend(&coin(6, 1_000))], 100, 1_000),
    )
    .await
    .expect("admitted");
    assert_eq!(mp.len(), 1);

    // the coin gets spent by the block that advances the peak
    store
        .apply_block(101, 0, &[], &[coin(6, 1_000).name()])
        .await
        .unwrap();
    let dropped = mp
        .new_peak(&store, 101, 0, &[coin(6, 1_000).name()])
        .await
        .unwrap();
    assert_eq!(dropped.dropped, 1);
    assert_eq!(mp.len(), 0);
    assert_eq!(mp.total_cost(), 0);
}

#[tokio::test]
async fn replacement_with_sufficient_fee_bump_evicts_conflict() {
    let store = store().await;
    let c = coin(0x40, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let old = mp
        .admit(&store, bundle(0xd1), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("original admitted");
    // +10M mojos over the evicted fee and a strictly higher fee-per-cost: qualifies.
    let new = mp
        .admit(
            &store,
            bundle(0xd2),
            conds(vec![mk_spend(&c)], 10_000_100, 1_000),
        )
        .await
        .expect("replacement admitted");
    assert_eq!(mp.len(), 1);
    assert!(mp.get(&old).is_none(), "evicted item must be gone");
    assert!(mp.get(&new).is_some());
}

#[tokio::test]
async fn replacement_must_spend_superset_of_conflicting_coins() {
    let store = store().await;
    let a = coin(0x41, 50_000_000);
    let b = coin(0x42, 50_000_000);
    store
        .apply_block(100, 0, &[record(a, 100), record(b, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    mp.admit(
        &store,
        bundle(0xd3),
        conds(vec![mk_spend(&a), mk_spend(&b)], 100, 1_000),
    )
    .await
    .expect("AB admitted");
    // Spending only A with a huge fee must NOT evict AB — that would drop B's spend from the pool
    // entirely (the attack the superset rule exists to prevent).
    let err = mp
        .admit(
            &store,
            bundle(0xd4),
            conds(vec![mk_spend(&a)], 20_000_000, 1_000),
        )
        .await
        .expect_err("non-superset replacement must be rejected");
    assert!(matches!(err, MempoolError::Conflict(_)));
    assert_eq!(mp.len(), 1);
}

#[tokio::test]
async fn replacement_rejected_at_equal_fee_per_cost() {
    let store = store().await;
    let c = coin(0x43, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    mp.admit(&store, bundle(0xd5), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("original admitted");
    let err = mp
        .admit(
            &store,
            bundle(0xd6),
            conds(vec![mk_spend(&c)], 10_000_100, 100_001_000),
        )
        .await
        .expect_err("equal fee-per-cost must be rejected");
    assert!(matches!(err, MempoolError::Conflict(_)));
}

#[tokio::test]
async fn replacement_must_preserve_timelocks() {
    let store = store().await;
    let c = coin(0x44, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    mp.admit(&store, bundle(0xd7), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("original admitted");
    let mut locked = conds(vec![mk_spend(&c)], 10_000_100, 1_000);
    locked.height_absolute = 123;
    let err = mp
        .admit(&store, bundle(0xd8), locked)
        .await
        .expect_err("future-timelocked replacement must park");
    assert!(matches!(err, MempoolError::Pending(..)));
    assert_eq!(mp.len(), 1);
}

#[tokio::test]
async fn future_assert_height_parks_and_revives_on_new_peak() {
    let store = store().await;
    let c = coin(0x50, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // ASSERT_HEIGHT_ABSOLUTE 105: five blocks in the future — parked, not resident, not lost.
    let mut conds_future = conds(vec![mk_spend(&c)], 100, 1_000);
    conds_future.height_absolute = 105;
    let bundle_future = bundle(0xe1);
    let name = bundle_future.name().expect("name");
    let err = mp
        .admit(&store, bundle_future, conds_future)
        .await
        .expect_err("future timelock must park");
    assert!(matches!(err, MempoolError::Pending(..)));
    assert!(mp.get(&name).is_none());

    let below = mp.new_peak(&store, 103, 0, &[]).await.expect("peak 103");
    assert!(below.admitted.is_empty());
    let still_below = mp.new_peak(&store, 104, 0, &[]).await.expect("peak 104");
    assert!(
        still_below.admitted.is_empty(),
        "assert 105 stays parked at peak 104 — a block on 104 could not carry it"
    );
    let at = mp.new_peak(&store, 105, 0, &[]).await.expect("peak 105");
    assert_eq!(at.admitted.len(), 1, "drains exactly at peak 105");
    assert_eq!(at.admitted[0].0, name);
    assert!(mp.get(&name).is_some());
}

// The invariant that makes GAP A a consensus bug: every RESIDENT item must
// validate in a block built on the admitting peak, per our own
// validate_block_conditions (ctx.block_height = the previous transaction
// block's height = the peak the mempool admitted against).
async fn assert_pool_valid_at_peak(mp: &Mempool, store: &SqliteStore, peak: u32, timestamp: u64) {
    for item in mp.items_by_fee() {
        let records = store.get_coin_records(&item.removals).await.unwrap();
        let coin_context = records
            .iter()
            .map(|r| {
                (
                    r.coin.name(),
                    CoinSpendContext {
                        birth_height: Some(r.confirmed_block_index),
                        birth_seconds: Some(r.timestamp),
                        spent_height: Some(r.confirmed_block_index),
                        spent_seconds: Some(r.timestamp),
                    },
                )
            })
            .collect();
        let ctx = ConditionValidationContext {
            block_height: peak,
            previous_transaction_block_timestamp: Some(timestamp),
            coin_context,
        };
        validate_block_conditions(&item.conds, &ctx).unwrap_or_else(|e| {
            panic!(
                "resident item {} must validate in a block built on peak {peak}: {e:?}",
                item.name
            )
        });
    }
}

#[tokio::test]
async fn assert_height_at_peak_plus_one_parks_until_the_peak_reaches_it() {
    let store = store().await;
    let c = coin(0x60, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut locked = conds(vec![mk_spend(&c)], 100, 1_000);
    locked.height_absolute = 101;
    let b = bundle(0xf1);
    let name = b.name().expect("name");
    let err = mp
        .admit(&store, b, locked)
        .await
        .expect_err("assert_height = peak+1 must park, not admit resident");
    assert!(matches!(err, MempoolError::Pending(..)), "got {err:?}");
    assert!(mp.get(&name).is_none());
    assert_pool_valid_at_peak(&mp, &store, 100, 0).await;

    let at = mp.new_peak(&store, 101, 0, &[]).await.expect("peak 101");
    assert_eq!(at.admitted.len(), 1, "drains at peak 101, not before");
    assert_eq!(at.admitted[0].0, name);
    assert!(mp.get(&name).is_some());
    assert_pool_valid_at_peak(&mp, &store, 101, 0).await;
}

#[tokio::test]
async fn unmet_assert_seconds_absolute_rejected_not_resident() {
    let store = store().await;
    let c = coin(0x61, 50_000_000);
    store
        .apply_block(100, 500_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    let mut locked = conds(vec![mk_spend(&c)], 100, 1_000);
    locked.seconds_absolute = 600_000; // peak timestamp 500_000 < 600_000: unmet
    let err = mp
        .admit(&store, bundle(0xf2), locked.clone())
        .await
        .expect_err("unmet ASSERT_SECONDS_ABSOLUTE must be rejected");
    assert!(
        matches!(err, MempoolError::TimelockNotMet(..)),
        "must fail outright (not park, not expire): got {err:?}"
    );
    assert_eq!(mp.len(), 0);
    let nothing = mp.new_peak(&store, 101, 550_000, &[]).await.expect("peak");
    assert!(nothing.admitted.is_empty(), "seconds locks never park");

    // The peak's timestamp catches up: the same bundle now admits resident.
    mp.set_peak(102, 600_000);
    mp.admit(&store, bundle(0xf2), locked)
        .await
        .expect("satisfied ASSERT_SECONDS_ABSOLUTE admits");
    assert_eq!(mp.len(), 1);
}

#[tokio::test]
async fn unmet_assert_seconds_relative_rejected_not_resident() {
    let store = store().await;
    let c = coin(0x66, 50_000_000);
    store
        .apply_block(100, 500_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    let mut spend = mk_spend(&c);
    spend.seconds_relative = Some(1_000); // effective 501_000 > peak timestamp 500_000
    let err = mp
        .admit(&store, bundle(0xf8), conds(vec![spend], 100, 1_000))
        .await
        .expect_err("unmet ASSERT_SECONDS_RELATIVE must be rejected");
    assert!(
        matches!(err, MempoolError::TimelockNotMet(..)),
        "got {err:?}"
    );
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn unmet_assert_height_relative_parks_and_drains_at_effective_height() {
    let store = store().await;
    let c = coin(0x62, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut spend = mk_spend(&c);
    spend.height_relative = Some(5); // effective assert height = 105
    let b = bundle(0xf3);
    let name = b.name().expect("name");
    let err = mp
        .admit(&store, b, conds(vec![spend], 100, 1_000))
        .await
        .expect_err("unmet ASSERT_HEIGHT_RELATIVE must park");
    assert!(matches!(err, MempoolError::Pending(..)), "got {err:?}");
    assert_pool_valid_at_peak(&mp, &store, 100, 0).await;

    let below = mp.new_peak(&store, 104, 0, &[]).await.expect("peak 104");
    assert!(
        below.admitted.is_empty(),
        "104 < effective 105: still parked"
    );
    let at = mp.new_peak(&store, 105, 0, &[]).await.expect("peak 105");
    assert_eq!(at.admitted.len(), 1, "drains at the effective height");
    assert_eq!(at.admitted[0].0, name);
    assert_pool_valid_at_peak(&mp, &store, 105, 0).await;
}

#[tokio::test]
async fn passed_assert_before_height_relative_rejects_outright() {
    let store = store().await;
    let c = coin(0x63, 50_000_000);
    store
        .apply_block(90, 0, &[record(c, 90)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut spend = mk_spend(&c);
    spend.before_height_relative = Some(5); // effective before-height 95 <= peak 100
    let err = mp
        .admit(&store, bundle(0xf4), conds(vec![spend], 100, 1_000))
        .await
        .expect_err("passed ASSERT_BEFORE_HEIGHT_RELATIVE must reject");
    assert!(matches!(err, MempoolError::Expired(..)), "got {err:?}");
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn impossible_height_constraints_reject_outright_not_park() {
    let store = store().await;
    let c = coin(0x64, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut locked = conds(vec![mk_spend(&c)], 100, 1_000);
    locked.height_absolute = 200;
    locked.before_height_absolute = Some(150); // before <= assert: impossible
    let err = mp
        .admit(&store, bundle(0xf5), locked)
        .await
        .expect_err("impossible constraints must reject");
    assert!(
        matches!(err, MempoolError::ImpossibleTimelock(..)),
        "must fail outright, not park: got {err:?}"
    );
    // Nothing parked: a future peak must not revive it.
    let later = mp.new_peak(&store, 300, 0, &[]).await.expect("peak 300");
    assert!(
        later.admitted.is_empty(),
        "impossible bundle must not revive"
    );
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn rbf_timelock_equality_compares_effective_values() {
    let store = store().await;
    let c = coin(0x65, 50_000_000);
    store
        .apply_block(90, 0, &[record(c, 90)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut spend = mk_spend(&c);
    spend.height_relative = Some(5); // effective assert height 95 <= peak: resident
    let old = mp
        .admit(&store, bundle(0xf6), conds(vec![spend], 100, 1_000))
        .await
        .expect("relative-locked original admitted resident");

    // Raw absolutes differ (95 vs 0) but the EFFECTIVE assert heights match.
    let mut replacement = conds(vec![mk_spend(&c)], 10_000_100, 1_000);
    replacement.height_absolute = 95;
    let new = mp
        .admit(&store, bundle(0xf7), replacement)
        .await
        .expect("equal effective timelocks + qualifying fee must replace");
    assert_eq!(mp.len(), 1);
    assert!(mp.get(&old).is_none(), "original evicted");
    assert!(mp.get(&new).is_some());

    let mut different = conds(vec![mk_spend(&c)], 20_000_200, 1_000);
    different.height_absolute = 96;
    let err = mp
        .admit(&store, bundle(0xf9), different)
        .await
        .expect_err("changed effective assert height must not replace");
    assert!(matches!(err, MempoolError::Conflict(_)), "got {err:?}");
    assert!(mp.get(&new).is_some(), "resident item untouched");
}

#[tokio::test]
async fn passed_assert_before_seconds_absolute_rejects_outright() {
    let store = store().await;
    let c = coin(0x67, 50_000_000);
    store
        .apply_block(100, 500_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    let mut expired = conds(vec![mk_spend(&c)], 100, 1_000);
    expired.before_seconds_absolute = Some(400_000); // peak timestamp 500_000 >= 400_000
    let err = mp
        .admit(&store, bundle(0xfa), expired)
        .await
        .expect_err("passed ASSERT_BEFORE_SECONDS_ABSOLUTE must reject");
    assert!(matches!(err, MempoolError::Expired(..)), "got {err:?}");
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn impossible_seconds_constraints_reject_outright() {
    let store = store().await;
    let c = coin(0x68, 50_000_000);
    store
        .apply_block(100, 500_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    let mut locked = conds(vec![mk_spend(&c)], 100, 1_000);
    locked.seconds_absolute = 900_000;
    locked.before_seconds_absolute = Some(900_000); // before <= assert: impossible
    let err = mp
        .admit(&store, bundle(0xfb), locked)
        .await
        .expect_err("impossible seconds constraints must reject");
    assert!(
        matches!(err, MempoolError::ImpossibleTimelock(..)),
        "got {err:?}"
    );
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn ephemeral_removal_uses_synthesized_peak_record() {
    let store = store().await;
    let a = coin(0x69, 1_000);
    store
        .apply_block(100, 500_000, &[record(a, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    // Spend A creating E, and spend E (ephemeral) with seconds_relative 0: admissible —
    // all spends land simultaneously at the synthesized peak timestamp.
    let e = Coin {
        parent_coin_info: a.name(),
        puzzle_hash: h(0x6a),
        amount: 900,
    };
    let mut spend_a = mk_spend(&a);
    spend_a.create_coin.insert(NewCoin {
        puzzle_hash: e.puzzle_hash,
        amount: e.amount,
        hint: None,
    });
    let mut spend_e = mk_spend(&e);
    spend_e.seconds_relative = Some(0);
    mp.admit(
        &store,
        bundle(0xfc),
        conds(vec![spend_a, spend_e], 1_000, 1_000),
    )
    .await
    .expect("ephemeral spend with ASSERT_SECONDS_RELATIVE 0 admits");
    assert_eq!(mp.len(), 1);

    let mut mp2 = Mempool::new(&MAINNET);
    mp2.set_peak(100, 500_000);
    let mut spend_a2 = mk_spend(&a);
    spend_a2.create_coin.insert(NewCoin {
        puzzle_hash: e.puzzle_hash,
        amount: e.amount,
        hint: None,
    });
    let mut spend_e2 = mk_spend(&e);
    spend_e2.height_relative = Some(1);
    let b2 = bundle(0xfd);
    let err = mp2
        .admit(&store, b2, conds(vec![spend_a2, spend_e2], 1_000, 1_000))
        .await
        .expect_err("ephemeral height_relative 1 must park");
    assert!(matches!(err, MempoolError::Pending(..)), "got {err:?}");
    let drain1 = mp2.new_peak(&store, 102, 500_000, &[]).await.expect("102");
    assert!(
        drain1.admitted.is_empty(),
        "recomputed at peak 102: re-parked"
    );
    let drain2 = mp2.new_peak(&store, 200, 500_000, &[]).await.expect("200");
    assert!(drain2.admitted.is_empty(), "still never admissible");
    assert_eq!(mp2.len(), 0);
}

#[tokio::test]
async fn new_peak_expires_resident_items_whose_before_bound_passed() {
    let store = store().await;
    let c1 = coin(0x6b, 50_000_000);
    let c2 = coin(0x6c, 50_000_000);
    store
        .apply_block(100, 500_000, &[record(c1, 100), record(c2, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 500_000);

    // Resident with ASSERT_BEFORE_HEIGHT_ABSOLUTE 105 — valid now, dead at peak 105.
    let mut before_height = conds(vec![mk_spend(&c1)], 100, 1_000);
    before_height.before_height_absolute = Some(105);
    let n1 = mp
        .admit(&store, bundle(0xfe), before_height)
        .await
        .expect("before-height item admitted while valid");
    // Resident with ASSERT_BEFORE_SECONDS_ABSOLUTE 600_000 — dead once the peak timestamp hits it.
    let mut before_seconds = conds(vec![mk_spend(&c2)], 100, 1_000);
    before_seconds.before_seconds_absolute = Some(600_000);
    let n2 = mp
        .admit(&store, bundle(0xff), before_seconds)
        .await
        .expect("before-seconds item admitted while valid");
    assert_eq!(mp.len(), 2);

    // Peak 104 @ 550_000: both bounds still ahead — nothing expires.
    let keep = mp.new_peak(&store, 104, 550_000, &[]).await.expect("104");
    assert_eq!(keep.expired, 0);
    assert_eq!(mp.len(), 2);

    // Peak 105 @ 600_000: both bounds passed (<= boundary on each side) — both expire.
    let sweep = mp.new_peak(&store, 105, 600_000, &[]).await.expect("105");
    assert_eq!(sweep.expired, 2, "both before-bounded items expire");
    assert!(mp.get(&n1).is_none());
    assert!(mp.get(&n2).is_none());
    assert_eq!(mp.len(), 0);
    assert_eq!(mp.total_cost(), 0);
}

#[tokio::test]
async fn passed_assert_before_rejects_outright() {
    let store = store().await;
    let c = coin(0x51, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let mut expired = conds(vec![mk_spend(&c)], 100, 1_000);
    expired.before_height_absolute = Some(90);
    let err = mp
        .admit(&store, bundle(0xe2), expired)
        .await
        .expect_err("passed ASSERT_BEFORE must reject");
    assert!(matches!(err, MempoolError::Expired(..)));
    assert_eq!(mp.len(), 0);
}

#[tokio::test]
async fn per_tx_cost_cap_is_half_max_block_cost() {
    let store = store().await;
    let c1 = coin(0x61, 1_000);
    let c2 = coin(0x62, 1_000);
    store
        .apply_block(100, 0, &[record(c1, 100), record(c2, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let err = mp
        .admit(
            &store,
            bundle(0x61),
            conds(vec![mk_spend(&c1)], 0, 6_000_000_000),
        )
        .await
        .expect_err("6B-cost tx exceeds the 5.5B per-tx mempool cap");
    match err {
        MempoolError::CostExceedsMax(c) => assert_eq!(c, 6_000_000_000),
        other => panic!("expected CostExceedsMax, got {other:?}"),
    }

    mp.admit(
        &store,
        bundle(0x62),
        conds(vec![mk_spend(&c2)], 0, 5_500_000_000),
    )
    .await
    .expect("5.5B-cost tx sits exactly at the cap");
}

#[tokio::test]
async fn capacity_ceiling_is_ten_blocks() {
    let mp = Mempool::new(&MAINNET);
    assert_eq!(
        mp.max_total_cost(),
        11_000_000_000 * 10,
        "mempool ceiling must be MAX_BLOCK_COST_CLVM x 10 (chia MEMPOOL_BLOCK_BUFFER)"
    );
}

// ---- Pool-full fee policy (mempool) ---------------------------------------------

// One resident item spending its own confirmed coin: (cost, fee) with everything else defaulted.
async fn seed_item(mp: &mut Mempool, store: &SqliteStore, tag: u8, fee: u64, cost: u64) -> Bytes32 {
    let c = coin(tag, u64::MAX / 2);
    store
        .apply_block(100, 1_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    mp.admit(store, bundle(tag), conds(vec![mk_spend(&c)], fee, cost))
        .await
        .expect("seed item admitted")
}

#[tokio::test]
async fn fee_above_mempool_item_fee_limit_rejected() {
    let store = store().await;
    let c = coin(0x63, u64::MAX / 2);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let over_limit = (1u64 << 50) + 1;
    let err = mp
        .admit(
            &store,
            bundle(0x63),
            conds(vec![mk_spend(&c)], over_limit, 1_000_000),
        )
        .await
        .expect_err("fee above 2^50 must reject INVALID_BLOCK_FEE_AMOUNT");
    assert_eq!(
        err.ack().1,
        "INVALID_BLOCK_FEE_AMOUNT",
        "got {err:?} instead"
    );

    mp.admit(
        &store,
        bundle(0x64),
        conds(vec![mk_spend(&c)], 1u64 << 50, 1_000_000),
    )
    .await
    .expect("fee exactly 2^50 admitted");
}

#[tokio::test]
async fn full_pool_rejects_fee_per_cost_below_nonzero_minimum() {
    let store = store().await;
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 1_000);
    // 20 items x 5.5B cost = 110B — exactly the 10-block ceiling, all at fpc 10.
    for i in 0..20u8 {
        seed_item(&mut mp, &store, 0x70 + i, 55_000_000_000, 5_500_000_000).await;
    }
    assert_eq!(mp.total_cost(), mp.max_total_cost(), "pool is exactly full");

    let c = coin(0x90, u64::MAX / 2);
    store
        .apply_block(100, 1_000, &[record(c, 100)], &[])
        .await
        .unwrap();
    // fpc 4 < 5: too close to zero, regardless of what it would evict.
    let err = mp
        .admit(
            &store,
            bundle(0x90),
            conds(vec![mk_spend(&c)], 4_000_000, 1_000_000),
        )
        .await
        .expect_err("full pool + fpc<5 must reject INVALID_FEE_TOO_CLOSE_TO_ZERO");
    assert_eq!(
        err.ack().1,
        "INVALID_FEE_TOO_CLOSE_TO_ZERO",
        "got {err:?} instead"
    );

    // fpc 7 clears the nonzero floor but not the pool's min fee rate (10) — INVALID_FEE_LOW_FEE.
    let err = mp
        .admit(
            &store,
            bundle(0x91),
            conds(vec![mk_spend(&c)], 7_000_000, 1_000_000),
        )
        .await
        .expect_err("full pool + fpc<=min_fee_rate must reject INVALID_FEE_LOW_FEE");
    assert_eq!(err.ack().1, "INVALID_FEE_LOW_FEE", "got {err:?} instead");

    // fpc 20 beats the resident min fee rate: admitted, evicting the lowest-priority resident.
    mp.admit(
        &store,
        bundle(0x92),
        conds(vec![mk_spend(&c)], 20_000_000, 1_000_000),
    )
    .await
    .expect("fpc above min fee rate displaces the pool");
    assert!(mp.total_cost() <= mp.max_total_cost());
}

#[tokio::test]
async fn is_fee_enough_mirrors_chia_pre_fetch_gate() {
    let store = store().await;
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 1_000);

    // Zero-cost is never enough.
    assert!(!mp.is_fee_enough(0, 0));
    // Room in the pool: even zero fee is enough.
    assert!(mp.is_fee_enough(0, 1_000_000));

    for i in 0..20u8 {
        seed_item(&mut mp, &store, 0x70 + i, 55_000_000_000, 5_500_000_000).await;
    }
    assert_eq!(mp.total_cost(), mp.max_total_cost());
    // Full: below the nonzero floor -> not enough.
    assert!(!mp.is_fee_enough(4_000_000, 1_000_000));
    // Full: equal to the resident min fee rate (10) -> not enough (must strictly beat it).
    assert!(!mp.is_fee_enough(10_000_000, 1_000_000));
    // Full: strictly above -> enough.
    assert!(mp.is_fee_enough(11_000_000, 1_000_000));
}

#[tokio::test]
async fn expiring_soon_items_capped_at_one_block_cost() {
    let store = store().await;
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 1_000);

    // Resident expiring-soon item: 5.5B cost at fpc 10, assert_before_height 120 (< 100+48).
    let c_a = coin(0xa0, u64::MAX / 2);
    store
        .apply_block(100, 1_000, &[record(c_a, 100)], &[])
        .await
        .unwrap();
    let mut a = conds(vec![mk_spend(&c_a)], 55_000_000_000, 5_500_000_000);
    a.before_height_absolute = Some(120);
    let a_name = mp
        .admit(&store, bundle(0xa0), a)
        .await
        .expect("first expiring item fits the budget");

    // Second expiring item at LOWER fpc (8): cumulative expiring cost would be 11B, over the
    // one-block budget (max_block_clvm_cost = 11B - block overhead) — and it cannot displace a
    // higher-priority expiring item, so it is rejected outright even though the pool has room.
    let c_b = coin(0xa1, u64::MAX / 2);
    store
        .apply_block(100, 1_000, &[record(c_b, 100)], &[])
        .await
        .unwrap();
    let mut b = conds(vec![mk_spend(&c_b)], 44_000_000_000, 5_500_000_000);
    b.before_height_absolute = Some(120);
    let err = mp
        .admit(&store, bundle(0xa1), b)
        .await
        .expect_err("expiring-soon beyond the one-block budget at lower priority must reject");
    assert_eq!(err.ack().1, "INVALID_FEE_LOW_FEE", "got {err:?} instead");

    // Same shape at HIGHER fpc (12): displaces the resident expiring item (EXPIRED eviction).
    let mut b2 = conds(vec![mk_spend(&c_b)], 66_000_000_000, 5_500_000_000);
    b2.before_height_absolute = Some(120);
    let b2_name = mp
        .admit(&store, bundle(0xa2), b2)
        .await
        .expect("higher-priority expiring item displaces the resident one");
    assert!(mp.get(&b2_name).is_some());
    assert!(
        mp.get(&a_name).is_none(),
        "displaced expiring-soon item must be evicted"
    );

    // A non-expiring item of the same size is untouched by the expiring budget.
    let c_c = coin(0xa2, u64::MAX / 2);
    store
        .apply_block(100, 1_000, &[record(c_c, 100)], &[])
        .await
        .unwrap();
    mp.admit(
        &store,
        bundle(0xa3),
        conds(vec![mk_spend(&c_c)], 44_000_000_000, 5_500_000_000),
    )
    .await
    .expect("non-expiring item unaffected by the expiring-soon budget");
}

#[tokio::test]
async fn conflict_losing_bundle_is_cached_not_dropped() {
    let store = store().await;
    let c = coin(0xb0, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // A wins the coin at admission.
    mp.admit(&store, bundle(0xb1), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("A admitted");
    // B double-spends the same coin at EQUAL fee-per-cost — no replace-by-fee, MEMPOOL_CONFLICT.
    let b_name = bundle(0xb2).name().expect("name");
    let err = mp
        .admit(&store, bundle(0xb2), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect_err("B conflicts with the resident item");
    assert!(matches!(err, MempoolError::Conflict(_)), "got {err:?}");

    assert_eq!(mp.conflict_cache_len(), 1, "losing conflict must be cached");
    assert_eq!(mp.conflict_cache_cost(), 1_000, "cache tracks its cost");
    assert!(mp.get(&b_name).is_none(), "B is only cached, not resident");
    assert_eq!(mp.len(), 1, "only A resident");
}

// Case 2 (resurrection): the winner LEAVES the pool unconfirmed (ASSERT_BEFORE_HEIGHT expiry, the
// coin never spent on-chain) → the coin is free again → the conflict drain re-admits the loser.
#[tokio::test]
async fn conflict_cached_bundle_readmits_after_winner_leaves_unconfirmed() {
    let store = store().await;
    let c = coin(0xb3, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // A wins the coin but carries ASSERT_BEFORE_HEIGHT 102 — it EXPIRES at peak 102 if unconfirmed,
    // leaving coin c unspent and spendable again.
    let a_name = bundle(0xb4).name().expect("name");
    let mut a_conds = conds(vec![mk_spend(&c)], 100, 1_000);
    a_conds.before_height_absolute = Some(102);
    mp.admit(&store, bundle(0xb4), a_conds)
        .await
        .expect("A admitted");

    // B loses the conflict at equal fee → cached.
    let b_name = bundle(0xb5).name().expect("name");
    mp.admit(&store, bundle(0xb5), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect_err("B conflicts");
    assert_eq!(mp.conflict_cache_len(), 1);

    // Peak 101: A neither expires (before 102) nor is its coin spent (spent=[]). B is drained,
    // re-admitted, RE-CONFLICTS with the still-resident A, and re-caches — no admission.
    let p101 = mp.new_peak(&store, 101, 0, &[]).await.expect("peak 101");
    assert!(
        p101.admitted.is_empty(),
        "A still resident → B still conflicts"
    );
    assert!(mp.get(&a_name).is_some(), "A resident at 101");
    assert_eq!(mp.conflict_cache_len(), 1, "B re-cached, still waiting");

    // Peak 102: A EXPIRES unconfirmed (ASSERT_BEFORE_HEIGHT 102 <= 102); coin c stays unspent
    // on-chain (spent=[]). The conflict drain re-admits B — the coin is free.
    let p102 = mp.new_peak(&store, 102, 0, &[]).await.expect("peak 102");
    assert!(mp.get(&a_name).is_none(), "A expired at 102");
    assert_eq!(
        p102.admitted.len(),
        1,
        "B resurrected from the conflict cache"
    );
    assert_eq!(p102.admitted[0].0, b_name);
    assert!(mp.get(&b_name).is_some(), "B now resident");
    assert_eq!(mp.conflict_cache_len(), 0, "conflict cache drained");
}

#[tokio::test]
async fn conflict_cache_evicts_oldest_past_the_cost_bound() {
    let store = store().await;
    let c = coin(0xb6, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // A wins c; every B below double-spends it at near-zero fee (no RBF) and loses.
    mp.admit(&store, bundle(0xc0), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("A admitted");

    // 0.4 block each (under the max_tx_cost = half-block cap); two fit, the third busts one block.
    let big = MAINNET.max_block_cost_clvm * 4 / 10;
    mp.admit(&store, bundle(0xc1), conds(vec![mk_spend(&c)], 100, big))
        .await
        .expect_err("b1 conflicts");
    mp.admit(&store, bundle(0xc2), conds(vec![mk_spend(&c)], 100, big))
        .await
        .expect_err("b2 conflicts");
    assert_eq!(
        mp.conflict_cache_len(),
        2,
        "b1,b2 both cached under the bound"
    );
    assert_eq!(mp.conflict_cache_cost(), big * 2);

    mp.admit(&store, bundle(0xc3), conds(vec![mk_spend(&c)], 100, big))
        .await
        .expect_err("b3 conflicts");

    assert_eq!(mp.conflict_cache_len(), 2, "oldest evicted, count held");
    assert_eq!(mp.conflict_cache_cost(), big * 2);
    assert!(
        mp.conflict_cache_cost() <= MAINNET.max_block_cost_clvm,
        "cache cost stays within one block"
    );
}

// Case 4: a cached loser whose coin got SPENT ON-CHAIN by the winner is a hard DOUBLE_SPEND on the
// drain — dropped, never resurrected, never re-cached (DOUBLE_SPEND is not the conflict path).
#[tokio::test]
async fn cached_conflict_dropped_when_winner_confirms_onchain() {
    let store = store().await;
    let c = coin(0xb8, 50_000_000);
    store
        .apply_block(100, 0, &[record(c, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    let a_name = bundle(0xd0).name().expect("name");
    mp.admit(&store, bundle(0xd0), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect("A admitted");
    let b_name = bundle(0xd1).name().expect("name");
    mp.admit(&store, bundle(0xd1), conds(vec![mk_spend(&c)], 100, 1_000))
        .await
        .expect_err("B conflicts");
    assert_eq!(mp.conflict_cache_len(), 1);

    // The winner CONFIRMS: block 101 spends coin c on-chain (A included in the block).
    store.apply_block(101, 0, &[], &[c.name()]).await.unwrap();
    let p = mp
        .new_peak(&store, 101, 0, &[c.name()])
        .await
        .expect("peak 101");

    // B's coin is spent on-chain now → DOUBLE_SPEND on the drain → dropped, not re-cached.
    assert!(mp.get(&a_name).is_none(), "A removed as a block inclusion");
    assert!(
        mp.get(&b_name).is_none(),
        "B not resurrected — its coin is spent on-chain"
    );
    assert!(
        !p.admitted.iter().any(|(n, _, _)| *n == b_name),
        "B not admitted"
    );
    assert_eq!(
        mp.conflict_cache_len(),
        0,
        "B dropped from the conflict cache, not re-cached"
    );
}

// Case 5 (independence): the ASSERT_HEIGHT PendingTxCache and the ConflictTxCache are
// separate structures — a pending-height drain admits its bundle while the conflict cache is
// retried untouched (its winner still resident), and vice versa.
#[tokio::test]
async fn pending_height_cache_and_conflict_cache_are_independent() {
    let store = store().await;
    let c_conf = coin(0xba, 50_000_000);
    let c_lock = coin(0xbb, 50_000_000);
    store
        .apply_block(100, 0, &[record(c_conf, 100), record(c_lock, 100)], &[])
        .await
        .unwrap();
    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // Conflict path: A wins c_conf, B loses → conflict cache.
    mp.admit(
        &store,
        bundle(0xe0),
        conds(vec![mk_spend(&c_conf)], 100, 1_000),
    )
    .await
    .expect("A admitted");
    mp.admit(
        &store,
        bundle(0xe1),
        conds(vec![mk_spend(&c_conf)], 100, 1_000),
    )
    .await
    .expect_err("B conflicts");
    assert_eq!(mp.conflict_cache_len(), 1);

    // Pending path: a bundle asserting height 101 parks (the PendingTxCache), NOT the conflict
    // cache.
    let locked_name = bundle(0xe2).name().expect("name");
    let mut locked = conds(vec![mk_spend(&c_lock)], 100, 1_000);
    locked.height_absolute = 101;
    mp.admit(&store, bundle(0xe2), locked)
        .await
        .expect_err("height-locked parks");
    assert_eq!(
        mp.conflict_cache_len(),
        1,
        "parking a height lock does not touch the conflict cache"
    );

    // Peak 101: the pending bundle drains and admits (height reached). The conflict cache is
    // retried too, but A is still resident so B re-caches — the two caches move independently.
    let p = mp.new_peak(&store, 101, 0, &[]).await.expect("peak 101");
    assert!(
        p.admitted.iter().any(|(n, _, _)| *n == locked_name),
        "pending bundle drained at its height"
    );
    assert!(
        mp.get(&locked_name).is_some(),
        "height-locked bundle now resident"
    );
    assert_eq!(
        mp.conflict_cache_len(),
        1,
        "conflict cache untouched by the pending drain — B still waiting"
    );
}

#[tokio::test]
async fn many_spend_bundle_is_deprioritized_by_the_spend_penalty() {
    let store = store().await;
    // One coin for the simple bundle, eight for the many-spend bundle — all confirmed.
    let simple_coin = coin(0x20, 1_000);
    let many_coins: Vec<Coin> = (0x10u8..0x18).map(|t| coin(t, 1_000)).collect();
    let mut records = vec![record(simple_coin, 100)];
    records.extend(many_coins.iter().map(|c| record(*c, 100)));
    store.apply_block(100, 0, &records, &[]).await.unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // Simple: 1 spend, cost 1_000_000, fee 700.
    //   raw fpc = 0.0007          virtual = 1_500_000 -> v-fpc ~= 0.000467
    // Many:   8 spends, cost 1_000_000, fee 800.
    //   raw fpc = 0.0008 (BEATS)  virtual = 5_000_000 -> v-fpc = 0.00016 (LOSES)
    let simple = mp
        .admit(
            &store,
            bundle(0xd1),
            conds(vec![mk_spend(&simple_coin)], 700, 1_000_000),
        )
        .await
        .expect("simple admitted");
    let many = mp
        .admit(
            &store,
            bundle(0xd2),
            conds(many_coins.iter().map(mk_spend).collect(), 800, 1_000_000),
        )
        .await
        .expect("many-spend admitted");

    let ordered = mp.items_by_fee();
    assert_eq!(
        ordered[0].name, simple,
        "priority is fee per VIRTUAL cost (chia 481ccb305, unconditional): the 8-spend bundle's \
         4M-cost spend penalty must outweigh its raw fee-per-cost edge"
    );
    assert_eq!(ordered[1].name, many);
    // The raw ordering really is inverted — this is the divergence the virtual key corrects.
    assert!(
        ordered[1].fee_per_cost() > ordered[0].fee_per_cost(),
        "precondition: the many-spend bundle wins on RAW fee-per-cost"
    );
}

#[tokio::test]
async fn pool_full_eviction_removes_lowest_virtual_cost_priority() {
    let store = store().await;
    const FILLERS: u64 = 20;
    let filler_coins: Vec<Coin> = (0x70u8..0x70 + FILLERS as u8)
        .map(|t| coin(t, u64::MAX / 2))
        .collect();
    let simple_coin = coin(0x31, 1_000);
    let incoming_coin = coin(0x32, u64::MAX / 2);
    let many_coins: Vec<Coin> = (0x40u8..0x48).map(|t| coin(t, 1_000)).collect();
    let mut records = vec![record(simple_coin, 100), record(incoming_coin, 100)];
    records.extend(filler_coins.iter().map(|c| record(*c, 100)));
    records.extend(many_coins.iter().map(|c| record(*c, 100)));
    store.apply_block(100, 0, &records, &[]).await.unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);

    // Fill to EXACTLY max_total_cost: high-fee fillers (each within the per-tx cost cap of
    // MAX_BLOCK_COST_CLVM / 2) plus the two 1M-cost candidates. 109_998_000_000 / 20 =
    // 5_499_900_000 exactly, under the 5.5B per-tx cap.
    let filler_cost = (mp.max_total_cost() - 2_000_000) / FILLERS;
    assert_eq!(
        filler_cost * FILLERS + 2_000_000,
        mp.max_total_cost(),
        "filler sizing divides the pool budget exactly"
    );
    let mut fillers = Vec::new();
    for (i, c) in filler_coins.iter().enumerate() {
        let name = mp
            .admit(
                &store,
                bundle(0xa0 + i as u8),
                // fpc 1.0 — far above both candidates, far below nothing that matters.
                conds(vec![mk_spend(c)], filler_cost, filler_cost),
            )
            .await
            .expect("filler admitted");
        fillers.push(name);
    }
    // Same (fee, cost, spends) shapes as the priority test: many wins RAW fpc, loses virtual.
    let simple = mp
        .admit(
            &store,
            bundle(0xe2),
            conds(vec![mk_spend(&simple_coin)], 700, 1_000_000),
        )
        .await
        .expect("simple admitted");
    let many = mp
        .admit(
            &store,
            bundle(0xe3),
            conds(many_coins.iter().map(mk_spend).collect(), 800, 1_000_000),
        )
        .await
        .expect("many-spend admitted");
    assert_eq!(mp.total_cost(), mp.max_total_cost(), "pool is exactly full");

    // Incoming 1M-cost item paying fpc 5.0: clears the nonzero floor (5) and the min fee rate
    // (both RAW-fpc gates, unchanged by 481ccb305), forcing a 1M-cost eviction.
    let incoming = mp
        .admit(
            &store,
            bundle(0xe4),
            conds(vec![mk_spend(&incoming_coin)], 5_000_000, 1_000_000),
        )
        .await
        .expect("incoming item displaces the lowest-priority resident");

    assert!(
        mp.get(&many).is_none(),
        "eviction victim is the lowest fee per VIRTUAL cost (chia 481ccb305, mempool.py:447-459): \
         the 8-spend bundle must go despite winning on RAW fee-per-cost"
    );
    assert!(mp.get(&simple).is_some(), "simple bundle survives");
    assert!(
        fillers.iter().all(|f| mp.get(f).is_some()),
        "high-fee fillers untouched"
    );
    assert!(mp.get(&incoming).is_some(), "incoming item resident");
    assert_eq!(
        mp.total_cost(),
        mp.max_total_cost(),
        "evicted exactly one 1M-cost item for the 1M-cost incoming"
    );
}

#[tokio::test]
async fn items_by_feerate_orders_by_raw_fee_per_cost() {
    let store = store().await;
    let simple_coin = coin(0x50, 1_000);
    let many_coins: Vec<Coin> = (0x60u8..0x68).map(|t| coin(t, 1_000)).collect();
    let mut records = vec![record(simple_coin, 100)];
    records.extend(many_coins.iter().map(|c| record(*c, 100)));
    store.apply_block(100, 0, &records, &[]).await.unwrap();

    let mut mp = Mempool::new(&MAINNET);
    mp.set_peak(100, 0);
    let simple = mp
        .admit(
            &store,
            bundle(0xf1),
            conds(vec![mk_spend(&simple_coin)], 700, 1_000_000),
        )
        .await
        .expect("simple admitted");
    let many = mp
        .admit(
            &store,
            bundle(0xf2),
            conds(many_coins.iter().map(mk_spend).collect(), 800, 1_000_000),
        )
        .await
        .expect("many-spend admitted");

    let by_feerate: Vec<Bytes32> = mp.items_by_feerate().iter().map(|i| i.name).collect();
    assert_eq!(
        by_feerate,
        vec![many, simple],
        "serve order is RAW fee-per-cost DESC (chia mempool.py:257-260): many-spend first"
    );
    let by_priority: Vec<Bytes32> = mp.items_by_fee().iter().map(|i| i.name).collect();
    assert_eq!(
        by_priority,
        vec![simple, many],
        "assembly order is virtual-cost priority: simple first — the two orders diverge"
    );
}
