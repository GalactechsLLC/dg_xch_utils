#![cfg(feature = "coin-index")]
use dg_xch_core::{
    blockchain::{coin::Coin, coin_record::CoinRecord, sized_bytes::Bytes32},
    protocols::explorer::CoinPageRequest,
};
use dg_xch_stores::{BlockStore, CoinStore, SqliteStore};

async fn contract(store: &(impl CoinStore + BlockStore + Sync)) {
    let ph = Bytes32::from([77; 32]);
    let mut adds = Vec::new();
    for i in 0..1003u32 {
        let mut parent = [0; 32];
        parent[..4].copy_from_slice(&i.to_be_bytes());
        adds.push(CoinRecord {
            coin: Coin {
                parent_coin_info: Bytes32::from(parent),
                puzzle_hash: ph,
                amount: if i < 2 { u64::MAX } else { 7 },
            },
            confirmed_block_index: 1,
            spent_block_index: 0,
            spent: false,
            coinbase: false,
            timestamp: 100,
        });
    }
    store.apply_block(1, 100, &adds, &[]).await.unwrap();
    store
        .apply_block(2, 110, &[], &[adds[0].coin.name()])
        .await
        .unwrap();
    store.build_indexes().await.unwrap();
    let summary = store.coin_summary(&ph, 2).await.unwrap();
    assert_eq!(
        summary.received,
        (2 * u128::from(u64::MAX) + 1001 * 7).to_string()
    );
    assert_eq!(summary.spent, u64::MAX.to_string());
    assert_eq!(
        summary.balance,
        (u128::from(u64::MAX) + 1001 * 7).to_string()
    );
    assert_eq!((summary.coin_count, summary.unspent_count), (1003, 1002));
    assert_eq!(summary.last_activity_height, Some(2));
    let early = store.coin_summary(&ph, 1).await.unwrap();
    assert_eq!(early.spent, "0");
    assert_eq!(early.unspent_count, 1003);
    let empty = store
        .coin_summary(&Bytes32::from([99; 32]), 2)
        .await
        .unwrap();
    assert_eq!(empty.balance, "0");
    assert_eq!(empty.first_seen, None);
    let mut req = CoinPageRequest {
        puzzle_hashes: vec![ph, ph],
        parent_ids: vec![],
        peak_hash: Bytes32::default(),
        offset: 0,
        limit: 25,
        spent: None,
    };
    let first = store.coin_page(&req, 2).await.unwrap();
    assert_eq!(first.len(), 25);
    assert!(first.windows(2).all(|pair|Vec::<u8>::from(pair[0].coin.name())>Vec::<u8>::from(pair[1].coin.name())));
    req.offset = 25;
    let second = store.coin_page(&req, 2).await.unwrap();
    assert!(
        first
            .iter()
            .all(|a| second.iter().all(|b| a.coin.name() != b.coin.name()))
    );
    req.offset = 0;
    req.spent = Some(true);
    assert_eq!(store.coin_page(&req, 2).await.unwrap().len(), 1);
    assert!(store.coin_page(&req, 1).await.unwrap().is_empty());
    req.spent = None;
    req.puzzle_hashes.clear();
    req.parent_ids = vec![adds[0].coin.parent_coin_info];
    assert_eq!(store.coin_page(&req, 2).await.unwrap().len(), 1);
    req.puzzle_hashes = vec![ph];
    assert!(store.coin_page(&req, 2).await.is_err());
    req.parent_ids.clear();
    req.limit = 501;
    assert!(store.coin_page(&req, 2).await.is_err());
    store.rollback_to(1).await.unwrap();
    assert_eq!(
        store.coin_summary(&ph, 1).await.unwrap().balance,
        summary.received
    );
}
#[tokio::test]
async fn sqlite_exact_summary_and_pages() {
    let temp = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&temp.path().join("fixture.sqlite"))
        .await
        .unwrap();
    contract(&store).await;
}
#[cfg(feature = "rocksdb")]
#[tokio::test]
async fn rocksdb_exact_summary_and_pages() {
    let temp = tempfile::tempdir().unwrap();
    let store = dg_xch_stores::RocksDbStore::open(temp.path())
        .await
        .unwrap();
    contract(&store).await;
}

#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "Requires an isolated fixture PostgreSQL database"]
async fn postgres_exact_summary_and_pages() {
    let url = std::env::var("NODE_FIXTURE_POSTGRES_URL").expect("Isolated fixture database URL");
    let store = dg_xch_stores::PostgresStore::open(&url).await.unwrap();
    contract(&store).await;
}
