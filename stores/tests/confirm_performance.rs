mod common;

use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_stores::types::{BlockStatus, CoinChanges};
use dg_xch_stores::{BlockStore, CoinStore};
use std::sync::atomic::Ordering;

fn coin(index: u64) -> CoinRecord {
    CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from([7; 32]),
            puzzle_hash: Bytes32::from([9; 32]),
            amount: index,
        },
        confirmed_block_index: 0,
        spent_block_index: 0,
        coinbase: false,
        timestamp: 0,
        spent: false,
    }
}

#[tokio::test]
async fn coin_lookup_batches_preserve_order_duplicates_and_missing_names() {
    let store = common::new_store().await;
    let additions: Vec<_> = (0..600).map(coin).collect();
    store.apply_block(3, 30, &additions, &[]).await.unwrap();
    let mut names: Vec<_> = additions
        .iter()
        .rev()
        .map(|record| record.coin.name())
        .collect();
    names.insert(256, coin(1000).coin.name());
    names.insert(512, additions[599].coin.name());
    names.push(additions[0].coin.name());
    let mut expected = Vec::new();
    for name in &names {
        if let Some(record) = store.get_coin_record(name).await.unwrap() {
            expected.push(record);
        }
    }
    let telemetry = store.telemetry().unwrap();
    let statements = telemetry.coin_lookup.statements.load(Ordering::Relaxed);
    let rows = telemetry.coin_lookup.rows.load(Ordering::Relaxed);
    assert_eq!(store.get_coin_records(&names).await.unwrap(), expected);
    assert_eq!(
        telemetry.coin_lookup.statements.load(Ordering::Relaxed) - statements,
        3
    );
    assert_eq!(
        telemetry.coin_lookup.rows.load(Ordering::Relaxed) - rows,
        names.len() as u64
    );
    assert!(store.get_coin_records(&[]).await.unwrap().is_empty());
    assert_eq!(
        telemetry.coin_lookup.statements.load(Ordering::Relaxed) - statements,
        3
    );
}

#[tokio::test]
async fn prepared_coins_do_not_wait_for_writer_and_preserve_atomic_spent_records() {
    let store = common::new_store().await;
    let additions: Vec<_> = (0..600).map(coin).collect();
    let removals: Vec<_> = additions[..300]
        .iter()
        .map(|record| record.coin.name())
        .collect();
    let changes = vec![dg_xch_stores::types::OwnedCoinChanges {
        height: 4,
        timestamp: 40,
        additions: additions.clone(),
        removals: removals.clone(),
        hints: vec![(Bytes32::from([3; 32]), additions[0].coin.name()); 2],
    }];
    let mut batch = store.begin().await.unwrap();
    let prepared = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.prepare_coin_window(changes.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    store
        .apply_prepared_coin_window_in(&mut batch, prepared)
        .await
        .unwrap();
    assert!(store.get_coin_record(&removals[0]).await.unwrap().is_none());
    drop(batch);
    let mut batch = store.begin().await.unwrap();
    assert!(store.get_coin_record(&removals[0]).await.unwrap().is_none());
    let prepared = store.prepare_coin_window(changes.clone()).await.unwrap();
    store
        .apply_prepared_coin_window_in(&mut batch, prepared)
        .await
        .unwrap();
    store.commit(batch).await.unwrap();
    let sequential = common::new_store().await;
    for change in &changes {
        sequential
            .apply_block(
                change.height,
                change.timestamp,
                &change.additions,
                &change.removals,
            )
            .await
            .unwrap();
    }
    let names: Vec<_> = additions.iter().map(|record| record.coin.name()).collect();
    assert_eq!(
        store.get_coin_records(&names).await.unwrap(),
        sequential.get_coin_records(&names).await.unwrap()
    );
    let record = store.get_coin_record(&removals[0]).await.unwrap().unwrap();
    assert!(record.spent);
    assert_eq!(record.confirmed_block_index, 4);
    assert_eq!(record.spent_block_index, 4);
    store.rollback_to(3).await.unwrap();
    assert!(store.get_coin_records(&names).await.unwrap().is_empty());
    assert!(
        store
            .telemetry()
            .unwrap()
            .coin_prepare_input_rows
            .load(Ordering::Relaxed)
            > store
                .telemetry()
                .unwrap()
                .coin_prepare_output_rows
                .load(Ordering::Relaxed)
    );
}

#[tokio::test]
async fn coalesced_coin_window_matches_sequential_writes_and_rollback() {
    let sequential = common::new_store().await;
    let coalesced = common::new_store().await;
    let initial = vec![coin(0), coin(1)];
    for store in [&sequential, &coalesced] {
        store.apply_block(1, 10, &initial, &[]).await.unwrap();
    }
    let additions: Vec<_> = (2..252).map(coin).collect();
    let first_removals = [initial[0].coin.name(), additions[0].coin.name()];
    let second_removals: Vec<_> = additions[1..151]
        .iter()
        .map(|record| record.coin.name())
        .collect();
    let replacement = [initial[0].clone()];
    let hints = [(Bytes32::from([3; 32]), additions[2].coin.name())];
    let changes = [
        CoinChanges {
            height: 2,
            timestamp: 20,
            additions: &additions,
            removals: &first_removals,
            hints: &hints,
        },
        CoinChanges {
            height: 3,
            timestamp: 30,
            additions: &[],
            removals: &second_removals,
            hints: &hints,
        },
        CoinChanges {
            height: 4,
            timestamp: 40,
            additions: &replacement,
            removals: &[],
            hints: &[],
        },
    ];
    let mut batch = sequential.begin().await.unwrap();
    for change in &changes {
        sequential
            .apply_block_in(
                &mut batch,
                change.height,
                change.timestamp,
                change.additions,
                change.removals,
            )
            .await
            .unwrap();
        sequential
            .apply_hints_in(&mut batch, change.hints)
            .await
            .unwrap();
    }
    sequential.commit(batch).await.unwrap();
    let mut batch = coalesced.begin().await.unwrap();
    let prepared = coalesced
        .prepare_coin_window(
            changes
                .iter()
                .map(|change| dg_xch_stores::types::OwnedCoinChanges {
                    height: change.height,
                    timestamp: change.timestamp,
                    additions: change.additions.to_vec(),
                    removals: change.removals.to_vec(),
                    hints: change.hints.to_vec(),
                })
                .collect(),
        )
        .await
        .unwrap();
    coalesced
        .apply_prepared_coin_window_in(&mut batch, prepared)
        .await
        .unwrap();
    coalesced.commit(batch).await.unwrap();
    let names: Vec<_> = initial
        .iter()
        .chain(&additions)
        .map(|record| record.coin.name())
        .collect();
    assert_eq!(
        sequential.get_coin_records(&names).await.unwrap(),
        coalesced.get_coin_records(&names).await.unwrap()
    );
    #[cfg(feature = "hint")]
    assert_eq!(
        sequential
            .get_coins_for_hint(&hints[0].0, 100)
            .await
            .unwrap(),
        coalesced
            .get_coins_for_hint(&hints[0].0, 100)
            .await
            .unwrap()
    );
    sequential.rollback_to(2).await.unwrap();
    coalesced.rollback_to(2).await.unwrap();
    assert_eq!(
        sequential.get_coin_records(&names).await.unwrap(),
        coalesced.get_coin_records(&names).await.unwrap()
    );
    let telemetry = coalesced.telemetry().unwrap();
    assert!(telemetry.coin_additions.statements.load(Ordering::Relaxed) < 10);
    assert!(telemetry.coin_removals.statements.load(Ordering::Relaxed) < 5);
}

#[tokio::test]
async fn prepared_archive_does_not_acquire_writer_and_is_atomic() {
    let store = common::new_store().await;
    let records = common::load_records();
    let body = common::load_full_block(5_000_000);
    let record = records
        .iter()
        .find(|record| record.height == body.height())
        .unwrap()
        .clone();
    let mut batch = store.begin().await.unwrap();
    let prepared = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.prepare_archive(
            vec![(record.clone(), BlockStatus::Validated)],
            vec![body.clone()],
        ),
    )
    .await
    .expect("preparation must not wait on the held writer")
    .unwrap();
    store
        .persist_prepared_archive_in(&mut batch, prepared)
        .await
        .unwrap();
    assert!(
        store
            .get_block_record(&record.header_hash)
            .await
            .unwrap()
            .is_none()
    );
    store.commit(batch).await.unwrap();
    assert_eq!(
        store.get_block(&record.header_hash).await.unwrap(),
        Some(body)
    );
    assert!(
        store
            .telemetry()
            .unwrap()
            .cache_writes
            .load(Ordering::Relaxed)
            > 0
    );
    assert_eq!(
        store.get_block_record(&record.header_hash).await.unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        store.get_status(&record.header_hash).await.unwrap(),
        BlockStatus::Validated
    );
    let mut batch = store.begin().await.unwrap();
    store
        .apply_coin_window_in(
            &mut batch,
            &[CoinChanges {
                height: 50,
                timestamp: 500,
                additions: &[coin(999)],
                removals: &[],
                hints: &[],
            }],
        )
        .await
        .unwrap();
    drop(batch);
    let batch = store.begin().await.unwrap();
    store.commit(batch).await.unwrap();
    assert!(
        store
            .get_coin_record(&coin(999).coin.name())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn custom_bulk_cache_survives_phase_changes() {
    let store = common::new_store().await;
    store.set_bulk_cache_kib(1024).await.unwrap();
    assert_eq!(store.writer_cache_size().await.unwrap(), -1024);
    store.set_near_tip(true);
    for _ in 0..100 {
        if store.writer_cache_size().await.unwrap() == -65536 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(store.writer_cache_size().await.unwrap(), -65536);
    store.set_near_tip(false);
    for _ in 0..100 {
        if store.writer_cache_size().await.unwrap() == -1024 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(store.writer_cache_size().await.unwrap(), -1024);
}
