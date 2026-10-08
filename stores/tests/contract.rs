mod common;

include!("common/block_contract_cases.rs");

#[tokio::test]
async fn deferred_index_build_creates_service_indexes() {
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;
    // Every secondary index is deferred out of open-time migration (the coin-record index-cost
    // report: during bulk sync they are pure write-amplification).
    assert!(!common::index_exists(&path, "coin_record_puzzle_hash").await);
    assert!(!common::index_exists(&path, "coin_record_confirmed_index").await);
    assert!(!common::index_exists(&path, "coin_record_spent_index").await);
    store.build_indexes().await.unwrap();
    assert!(
        common::index_exists(&path, "coin_record_confirmed_index").await,
        "reorg indexes build on every profile"
    );
    assert!(
        common::index_exists(&path, "coin_record_spent_index").await,
        "reorg indexes build on every profile"
    );
    #[cfg(feature = "coin-index")]
    assert!(
        common::index_exists(&path, "coin_record_puzzle_hash").await,
        "service indexes build under coin-index"
    );
    #[cfg(not(feature = "coin-index"))]
    assert!(
        !common::index_exists(&path, "coin_record_puzzle_hash").await,
        "a validator never builds service indexes"
    );
}

#[tokio::test]
async fn falling_edge_shed_drops_secondary_indexes_and_build_restores_them() {
    use dg_xch_stores::CoinStore;
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;
    store.build_indexes().await.unwrap();
    assert!(common::index_exists(&path, "coin_record_confirmed_index").await);
    assert!(common::index_exists(&path, "coin_record_spent_index").await);

    store.shed_service_indexes().await.unwrap();
    for idx in [
        "coin_record_confirmed_index",
        "coin_record_spent_index",
        "coin_record_puzzle_hash",
        "coin_record_coin_parent",
        "coin_record_unspent_by_ph",
    ] {
        assert!(
            !common::index_exists(&path, idx).await,
            "{idx} must be shed for deep re-catch-up"
        );
    }
    #[cfg(feature = "hint")]
    assert!(
        !common::index_exists(&path, "coin_hint_coin_name").await,
        "the coin_hint secondary phases with the service tier"
    );

    // A reorg requested while shed still works: ensure_reorg_indexes rebuilds the reorg tier on
    // demand, then the rollback runs over it.
    let (adds, _) = common::load_adds_rems(5_000_000);
    store.apply_block(10, 0, &adds[..4], &[]).await.unwrap();
    store.ensure_reorg_indexes().await.unwrap();
    assert!(common::index_exists(&path, "coin_record_confirmed_index").await);
    assert!(common::index_exists(&path, "coin_record_spent_index").await);
    assert_eq!(
        store.rollback_to(9).await.unwrap(),
        4,
        "rollback over the on-demand indexes reverts the applied coins"
    );

    // The rising edge restores the full set on top of whatever subset an interrupted shed left.
    store.build_indexes().await.unwrap();
    assert!(common::index_exists(&path, "coin_record_confirmed_index").await);
    assert!(common::index_exists(&path, "coin_record_spent_index").await);
    #[cfg(feature = "coin-index")]
    for idx in [
        "coin_record_puzzle_hash",
        "coin_record_coin_parent",
        "coin_record_unspent_by_ph",
    ] {
        assert!(
            common::index_exists(&path, idx).await,
            "{idx} must be restored by the rising-edge build"
        );
    }
    #[cfg(feature = "hint")]
    assert!(
        common::index_exists(&path, "coin_hint_coin_name").await,
        "the coin_hint secondary rebuilds with the service tier"
    );
}
