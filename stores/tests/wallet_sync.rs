use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::protocols::wallet_sync::WalletCoinRecordsRequest;
use dg_xch_stores::{CoinStore, SqliteStore};
use std::sync::Arc;

#[tokio::test]
async fn sqlite_capabilities_forward_through_arc() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(&temp.path().join("capabilities.sqlite"))
            .await
            .unwrap(),
    );
    assert_eq!(store.supports_wallet_sync(), cfg!(feature = "hint"));
    assert_eq!(
        store.supports_explorer_queries(),
        cfg!(feature = "coin-index")
    );
    if !store.supports_wallet_sync() {
        let request = WalletCoinRecordsRequest {
            puzzle_hashes: vec![Bytes32::default()],
            hints: vec![],
            peak_hash: Bytes32::default(),
            cursor: None,
            limit: 1,
        };
        assert!(matches!(store.wallet_coin_records(&request, 0).await,
            Err(dg_xch_stores::StoreError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported));
    }
}

#[cfg(feature = "hint")]
async fn contract(store: &(impl CoinStore + dg_xch_stores::BlockStore + Sync)) {
    use dg_xch_core::blockchain::{coin::Coin, coin_record::CoinRecord};
    use dg_xch_core::traits::SizedBytes;

    assert!(store.supports_wallet_sync());
    let ph_a = Bytes32::from([171; 32]);
    let ph_b = Bytes32::from([172; 32]);
    let unrelated = Bytes32::from([173; 32]);
    let hint = Bytes32::from([174; 32]);
    let peak = Bytes32::from([175; 32]);
    let mut additions = Vec::new();
    let mut hints = Vec::new();
    let mut expected = Vec::new();
    for i in 0..1207_u32 {
        let mut parent = [176; 32];
        parent[..4].copy_from_slice(&i.to_be_bytes());
        let record = CoinRecord {
            coin: Coin {
                parent_coin_info: Bytes32::from(parent),
                puzzle_hash: match i % 4 {
                    0 => ph_a,
                    2 => ph_b,
                    _ => unrelated,
                },
                amount: u64::MAX - u64::from(i),
            },
            confirmed_block_index: 1,
            spent_block_index: 0,
            coinbase: i == 0,
            timestamp: 100,
            spent: false,
        };
        if i % 4 <= 1 {
            // Every fourth coin overlaps hash and hint discovery; another is hint-only.
            hints.push((hint, record.coin.name()));
        }
        if i % 4 != 3 {
            let mut at_height_two = record;
            if i < 2 {
                at_height_two.spent_block_index = 2;
                at_height_two.spent = true;
            }
            expected.push(at_height_two);
        }
        additions.push(record);
    }
    store.apply_block(1, 100, &additions, &[]).await.unwrap();
    hints.push((hint, Bytes32::from([177; 32]))); // Dangling hints cannot produce records.
    store.apply_hints(&hints).await.unwrap();
    store
        .apply_block(
            2,
            110,
            &[],
            &[additions[0].coin.name(), additions[1].coin.name()],
        )
        .await
        .unwrap();
    let future = CoinRecord {
        coin: Coin {
            parent_coin_info: Bytes32::from([178; 32]),
            puzzle_hash: ph_a,
            amount: 19,
        },
        confirmed_block_index: 3,
        timestamp: 120,
        ..additions[2]
    };
    store
        .apply_block(3, 120, &[future], &[additions[2].coin.name()])
        .await
        .unwrap();
    store
        .apply_hints(&[(hint, future.coin.name())])
        .await
        .unwrap();
    store.build_indexes().await.unwrap();
    expected.sort_by_key(|record| record.coin.name().bytes());

    let mut request = WalletCoinRecordsRequest {
        puzzle_hashes: vec![ph_b, ph_a, ph_a],
        hints: vec![hint, hint],
        peak_hash: peak,
        cursor: None,
        limit: 137,
    };
    let mut actual = Vec::new();
    loop {
        let response = store.wallet_coin_records(&request, 2).await.unwrap();
        assert!(response.success);
        assert_eq!((response.peak_hash, response.height), (peak, 2));
        assert!(response.coin_records.len() <= request.limit as usize);
        let remaining = expected.len() - actual.len();
        assert_eq!(
            response.coin_records.len(),
            remaining.min(request.limit as usize)
        );
        assert_eq!(
            response.next_cursor.is_some(),
            remaining > request.limit as usize
        );
        if let Some(next) = response.next_cursor {
            assert_eq!(
                Some(next),
                response
                    .coin_records
                    .last()
                    .map(|record| record.coin.name())
            );
            assert!(
                request
                    .cursor
                    .is_none_or(|previous| next.bytes() > previous.bytes())
            );
        }
        actual.extend(response.coin_records);
        request.cursor = response.next_cursor;
        if request.cursor.is_none() {
            break;
        }
        assert!(
            actual.len() <= expected.len(),
            "pagination must make progress"
        );
    }
    assert_eq!(
        actual, expected,
        "spent history, hint-only coins, and hash/hint overlaps must appear exactly once"
    );

    // A full final page must not invent a continuation cursor.
    request.cursor = Some(expected[expected.len() - 138].coin.name());
    let final_page = store.wallet_coin_records(&request, 2).await.unwrap();
    assert_eq!(final_page.coin_records, expected[expected.len() - 137..]);
    assert_eq!(final_page.next_cursor, None);
    request.cursor = Some(Bytes32::from([255; 32]));
    let empty = store.wallet_coin_records(&request, 2).await.unwrap();
    assert!(empty.coin_records.is_empty());
    assert_eq!(empty.next_cursor, None);

    // Both selector types also work alone, including a nonexistent cursor between IDs.
    request.cursor = None;
    request.puzzle_hashes.clear();
    request.hints = vec![hint];
    request.limit = 500;
    let hinted = store.wallet_coin_records(&request, 2).await.unwrap();
    let expected_hinted: Vec<_> = expected
        .iter()
        .copied()
        .filter(|record| record.coin.puzzle_hash == ph_a || record.coin.puzzle_hash == unrelated)
        .collect();
    assert_eq!(hinted.coin_records, expected_hinted[..500]);
    assert_eq!(hinted.next_cursor, Some(expected_hinted[499].coin.name()));
    request.hints.clear();
    request.puzzle_hashes = vec![ph_b];
    let hash_only = store.wallet_coin_records(&request, 2).await.unwrap();
    let expected_hash: Vec<_> = expected
        .iter()
        .copied()
        .filter(|record| record.coin.puzzle_hash == ph_b)
        .collect();
    assert_eq!(hash_only.coin_records, expected_hash);
    assert_eq!(hash_only.next_cursor, None);
    request.cursor = Some(Bytes32::from([128; 32]));
    let after_gap = store.wallet_coin_records(&request, 2).await.unwrap();
    assert_eq!(
        after_gap.coin_records,
        expected_hash
            .into_iter()
            .filter(|record| record.coin.name().bytes() > [128; 32])
            .collect::<Vec<_>>()
    );

    // The maximum selector count remains a bounded query, even with empty matches.
    request.cursor = None;
    request.puzzle_hashes = (0..99_u8).map(|n| Bytes32::from([n; 32])).collect();
    request.hints = vec![hint];
    request.limit = 1;
    let many = store.wallet_coin_records(&request, 2).await.unwrap();
    assert_eq!(many.coin_records, expected_hinted[..1]);
    assert_eq!(many.next_cursor, Some(expected_hinted[0].coin.name()));
    request.puzzle_hashes.push(ph_a);
    assert!(store.wallet_coin_records(&request, 2).await.is_err());
    request.puzzle_hashes.clear();
    for limit in [0, 501, u32::MAX] {
        request.limit = limit;
        assert!(store.wallet_coin_records(&request, 2).await.is_err());
    }
    request.hints.clear();
    request.limit = 1;
    assert!(store.wallet_coin_records(&request, 2).await.is_err());
}

#[cfg(feature = "hint")]
#[tokio::test]
async fn sqlite_wallet_history_pages() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(&temp.path().join("wallet.sqlite"))
            .await
            .unwrap(),
    );
    contract(&store).await;
}

#[cfg(all(feature = "hint", feature = "postgres"))]
#[tokio::test]
#[ignore = "Requires an isolated fixture PostgreSQL database"]
async fn postgres_wallet_history_pages() {
    let url = std::env::var("NODE_FIXTURE_POSTGRES_URL").expect("Isolated fixture database URL");
    let store = Arc::new(dg_xch_stores::PostgresStore::open(&url).await.unwrap());
    contract(&store).await;
}
