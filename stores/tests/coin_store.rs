mod common;

include!("common/coin_contract_cases.rs");

#[cfg(not(feature = "coin-index"))]
#[tokio::test]
async fn service_index_absent_for_a_validator() {
    let path = common::unique_db_path();
    let _store = common::new_store_at(&path).await;
    assert!(
        !common::index_exists(&path, "coin_record_puzzle_hash").await,
        "puzzle_hash index must be absent without the coin-index feature"
    );
}

#[cfg(feature = "coin-index")]
#[tokio::test]
async fn service_index_present_when_enabled() {
    use dg_xch_stores::BlockStore;
    let path = common::unique_db_path();
    let store = common::new_store_at(&path).await;
    assert!(
        !common::index_exists(&path, "coin_record_puzzle_hash").await,
        "service indexes are deferred: absent at open even with coin-index"
    );
    store.build_indexes().await.unwrap();
    assert!(
        common::index_exists(&path, "coin_record_puzzle_hash").await,
        "puzzle_hash index must exist after the deferred build"
    );
}
