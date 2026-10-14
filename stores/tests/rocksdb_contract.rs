#![cfg(feature = "rocksdb")]

#[path = "common/mod.rs"]
mod fixtures;

mod common {
    pub use super::fixtures::{load_adds_rems, load_full_block, load_records};
    pub async fn new_store() -> dg_xch_stores::RocksDbStore {
        dg_xch_stores::RocksDbStore::open(
            &super::fixtures::unique_db_path().with_extension("rocksdb"),
        )
        .await
        .expect("open rocksdb")
    }
}

mod blocks {
    use super::common;
    include!("common/block_contract_cases.rs");
}
mod coins {
    use super::common;
    include!("common/coin_contract_cases.rs");
}

use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_stores::{BlockStatus, BlockStore, CoinStore, RocksDbStore, SqliteStore};

fn linked(template: &BlockRecord, tag: u8, h: u32, parent: Bytes32) -> BlockRecord {
    let mut record = template.clone();
    record.header_hash = Bytes32::from([tag; 32]);
    record.prev_hash = parent;
    record.height = h;
    record
}

#[tokio::test]
async fn reorg_replay_and_indexes_match_sqlite_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let rock_path = dir.path().join("rocks");
    let rocks = RocksDbStore::open(&rock_path).await.unwrap();
    let sql = SqliteStore::open(&dir.path().join("sqlite.db"))
        .await
        .unwrap();
    let template = &fixtures::load_records()[0];
    let root = linked(template, 1, 1, Bytes32::from([0; 32]));
    let old = linked(template, 2, 2, root.header_hash);
    let old_top = linked(template, 3, 3, old.header_hash);
    let new = linked(template, 4, 2, root.header_hash);
    let records = [root.clone(), old, old_top.clone(), new.clone()];
    let (coins, _) = fixtures::load_adds_rems(5_000_000);
    for store in [&rocks as &dyn StorePair, &sql as &dyn StorePair] {
        store.add_block_records(&records).await.unwrap();
        store.apply_block(1, 100, &coins[..1], &[]).await.unwrap();
        store
            .apply_block(2, 200, &coins[1..2], &[coins[0].coin.name()])
            .await
            .unwrap();
        store.set_peak(&old_top.header_hash).await.unwrap();
        let mut batch = store.begin().await.unwrap();
        assert_eq!(store.rollback_to_in(&mut batch, 1).await.unwrap(), 2);
        store
            .apply_block_in(&mut batch, 2, 300, &coins[2..3], &[coins[0].coin.name()])
            .await
            .unwrap();
        store
            .set_status_in(&mut batch, &new.header_hash, BlockStatus::Validated)
            .await
            .unwrap();
        assert_eq!(
            store
                .set_peak_in(&mut batch, &new.header_hash)
                .await
                .unwrap(),
            1
        );
        store.commit(batch).await.unwrap();
    }
    let ids: Vec<_> = coins[..3].iter().map(|r| r.coin.name()).collect();
    assert_eq!(
        rocks.get_coin_records(&ids).await.unwrap(),
        sql.get_coin_records(&ids).await.unwrap()
    );
    assert_eq!(
        rocks.get_peak().await.unwrap(),
        sql.get_peak().await.unwrap()
    );
    for h in 0..=4 {
        assert_eq!(
            rocks.get_block_record_by_height(h).await.unwrap(),
            sql.get_block_record_by_height(h).await.unwrap()
        );
    }
    #[cfg(feature = "coin-index")]
    {
        assert_eq!(
            rocks.get_coins_added_at_height(2).await.unwrap(),
            sql.get_coins_added_at_height(2).await.unwrap()
        );
        assert_eq!(
            rocks.get_coins_removed_at_height(2).await.unwrap(),
            sql.get_coins_removed_at_height(2).await.unwrap()
        );
    }
    drop(rocks);
    let reopened = RocksDbStore::open(&rock_path).await.unwrap();
    assert_eq!(
        reopened.get_peak().await.unwrap(),
        Some((new.header_hash, 2))
    );
    assert_eq!(
        reopened.get_status(&new.header_hash).await.unwrap(),
        BlockStatus::Validated
    );
    assert_eq!(
        reopened.get_coin_records(&ids).await.unwrap(),
        sql.get_coin_records(&ids).await.unwrap()
    );
    assert!(
        reopened
            .get_block_record_by_height(3)
            .await
            .unwrap()
            .is_none()
    );
}

trait StorePair: BlockStore + CoinStore + Send + Sync {}
impl<T: BlockStore + CoinStore + Send + Sync> StorePair for T {}

#[tokio::test]
async fn dropped_and_failed_batches_never_publish_and_wrong_instance_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = RocksDbStore::open(&dir.path().join("one")).await.unwrap();
    let other = RocksDbStore::open(&dir.path().join("two")).await.unwrap();
    let records = fixtures::load_records();
    let (coins, _) = fixtures::load_adds_rems(5_000_000);
    let mut batch = store.begin().await.unwrap();
    store
        .add_block_records_in(&mut batch, &records)
        .await
        .unwrap();
    store
        .apply_block_in(&mut batch, 10, 100, &coins[..1], &[])
        .await
        .unwrap();
    assert!(
        other
            .set_peak_in(&mut batch, &records[0].header_hash)
            .await
            .is_err()
    );
    assert!(
        store
            .get_block_record(&records[0].header_hash)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_coin_record(&coins[0].coin.name())
            .await
            .unwrap()
            .is_none()
    );
    drop(batch);
    let mut batch = store.begin().await.unwrap();
    store
        .apply_block_in(&mut batch, 10, 100, &coins[..1], &[])
        .await
        .unwrap();
    assert!(
        store
            .append_many(&mut batch, &[fixtures::load_full_block(5_000_000)])
            .await
            .is_err()
    );
    assert!(store.commit(batch).await.is_err());
    assert!(
        store
            .get_coin_record(&coins[0].coin.name())
            .await
            .unwrap()
            .is_none()
    );
    // Failure releases the writer and leaves the store usable.
    store.add_block_records(&records).await.unwrap();
}

#[tokio::test]
async fn prepared_windows_preserve_ephemeral_coins_and_record_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let store = RocksDbStore::open(&dir.path().join("rocks")).await.unwrap();
    let record = fixtures::load_records()[0].clone();
    let block = fixtures::load_full_block(5_000_000);
    let (coins, _) = fixtures::load_adds_rems(5_000_000);
    let id = coins[0].coin.name();
    let prepared = store
        .prepare_archive(vec![(record.clone(), BlockStatus::Validated)], vec![block])
        .await
        .unwrap();
    let changes = store
        .prepare_coin_window(vec![
            dg_xch_stores::types::OwnedCoinChanges {
                height: 10,
                timestamp: 100,
                additions: coins[..1].to_vec(),
                removals: vec![id],
                hints: vec![],
            },
            dg_xch_stores::types::OwnedCoinChanges {
                height: 11,
                timestamp: 110,
                additions: vec![],
                removals: vec![id],
                hints: vec![],
            },
        ])
        .await
        .unwrap();
    let mut batch = store.begin().await.unwrap();
    store
        .persist_prepared_archive_in(&mut batch, prepared)
        .await
        .unwrap();
    store
        .apply_prepared_coin_window_in(&mut batch, changes)
        .await
        .unwrap();
    store
        .set_peak_in(&mut batch, &record.header_hash)
        .await
        .unwrap();
    store.commit(batch).await.unwrap();
    store
        .add_block_records(std::slice::from_ref(&record))
        .await
        .unwrap();
    assert_eq!(
        store.get_status(&record.header_hash).await.unwrap(),
        BlockStatus::Validated
    );
    assert_eq!(
        store
            .get_block_record_by_height(record.height)
            .await
            .unwrap(),
        Some(record)
    );
    let coin = store.get_coin_record(&id).await.unwrap().unwrap();
    assert_eq!(
        (
            coin.confirmed_block_index,
            coin.spent_block_index,
            coin.spent
        ),
        (10, 11, true)
    );
}

#[tokio::test]
async fn segments_replace_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rocks");
    let id = Bytes32::from([77; 32]);
    let store = RocksDbStore::open(&path).await.unwrap();
    assert!(store.get_sub_epoch_segments(&id).await.unwrap().is_none());
    store.persist_sub_epoch_segments(&id, b"old").await.unwrap();
    store.persist_sub_epoch_segments(&id, b"new").await.unwrap();
    drop(store);
    assert_eq!(
        RocksDbStore::open(&path)
            .await
            .unwrap()
            .get_sub_epoch_segments(&id)
            .await
            .unwrap(),
        Some(b"new".to_vec())
    );
}

#[test]
fn crash_writer_child() {
    let Some(path) = std::env::var_os("DGX_ROCKSDB_TEST_CRASH_PATH") else {
        return;
    };
    let commit = std::env::var_os("DGX_ROCKSDB_TEST_CRASH_COMMIT").is_some();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store = RocksDbStore::open(std::path::Path::new(&path))
            .await
            .unwrap();
        let records = fixtures::load_records();
        let (coins, _) = fixtures::load_adds_rems(5_000_000);
        let mut batch = store.begin().await.unwrap();
        store.rollback_to_in(&mut batch, 9).await.unwrap();
        store
            .apply_block_in(&mut batch, 11, 110, &coins[1..2], &[])
            .await
            .unwrap();
        store
            .set_status_in(&mut batch, &records[1].header_hash, BlockStatus::Validated)
            .await
            .unwrap();
        store
            .set_peak_in(&mut batch, &records[1].header_hash)
            .await
            .unwrap();
        if commit {
            store.commit(batch).await.unwrap();
        }
        // Exit without dropping handles or running RocksDB's graceful shutdown.
        std::process::exit(23);
    });
}

#[tokio::test]
async fn abrupt_process_exit_recovers_the_complete_old_or_new_reorg() {
    let dir = tempfile::tempdir().unwrap();
    let records = fixtures::load_records();
    let (coins, _) = fixtures::load_adds_rems(5_000_000);
    for commit in [false, true] {
        let path = dir
            .path()
            .join(if commit { "committed" } else { "uncommitted" });
        let store = RocksDbStore::open(&path).await.unwrap();
        store.add_block_records(&records).await.unwrap();
        store.apply_block(10, 100, &coins[..1], &[]).await.unwrap();
        store.set_peak(&records[0].header_hash).await.unwrap();
        drop(store);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "crash_writer_child", "--nocapture"])
            .env("DGX_ROCKSDB_TEST_CRASH_PATH", &path);
        if commit {
            child.env("DGX_ROCKSDB_TEST_CRASH_COMMIT", "1");
        }
        let status = child.status().unwrap();
        assert_eq!(status.code(), Some(23), "child must reach the crash point");
        let store = RocksDbStore::open(&path).await.unwrap();
        assert_eq!(
            store
                .get_coin_record(&coins[0].coin.name())
                .await
                .unwrap()
                .is_some(),
            !commit
        );
        assert_eq!(
            store
                .get_coin_record(&coins[1].coin.name())
                .await
                .unwrap()
                .is_some(),
            commit
        );
        let expected = &records[usize::from(commit)];
        assert_eq!(
            store.get_peak().await.unwrap(),
            Some((expected.header_hash, expected.height))
        );
        assert_eq!(
            store.get_status(&records[1].header_hash).await.unwrap(),
            if commit {
                BlockStatus::Validated
            } else {
                BlockStatus::Unvalidated
            }
        );
    }
}

#[cfg(feature = "coin-index")]
#[tokio::test]
async fn coin_replacement_removes_old_height_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let store = RocksDbStore::open(&dir.path().join("rocks")).await.unwrap();
    let (coins, _) = fixtures::load_adds_rems(5_000_000);
    let id = coins[0].coin.name();
    store.apply_block(2, 20, &coins[..1], &[]).await.unwrap();
    store.apply_block(3, 30, &[], &[id]).await.unwrap();
    store.apply_block(4, 40, &coins[..1], &[]).await.unwrap();
    assert!(store.get_coins_added_at_height(2).await.unwrap().is_empty());
    assert!(
        store
            .get_coins_removed_at_height(3)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.get_coins_added_at_height(4).await.unwrap().len(), 1);
    assert!(!store.get_coin_record(&id).await.unwrap().unwrap().spent);
    #[cfg(feature = "hint")]
    {
        let hint = Bytes32::from([123; 32]);
        store.apply_hints(&[(hint, id), (hint, id)]).await.unwrap();
        assert_eq!(store.get_coins_for_hint(&hint, 10).await.unwrap(), vec![id]);
        store.rollback_to(3).await.unwrap();
        assert_eq!(store.get_coins_for_hint(&hint, 10).await.unwrap(), vec![id]);
        assert!(
            store
                .get_coin_records_by_hint(&hint, true)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn schema_and_feature_mismatches_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rocks");
    drop(RocksDbStore::open(&path).await.unwrap());
    {
        let raw = rocksdb::DB::open_default(&path).unwrap();
        raw.put(b"!schema", b"999").unwrap();
    }
    assert!(RocksDbStore::open(&path).await.is_err());
    {
        let raw = rocksdb::DB::open_default(&path).unwrap();
        raw.put(b"!schema", b"1").unwrap();
        raw.put(
            b"!features",
            [
                u8::from(!cfg!(feature = "coin-index")),
                u8::from(!cfg!(feature = "hint")),
            ],
        )
        .unwrap();
    }
    assert!(RocksDbStore::open(&path).await.is_err());
}
