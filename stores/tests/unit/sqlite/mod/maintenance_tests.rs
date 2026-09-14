use super::*;
use crate::{BlockStore, CoinStore};

#[tokio::test]
async fn maintenance_has_an_isolated_disk_backed_sort_profile() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("chain.db"))
        .await
        .unwrap();
    let mut maintenance = store.maintenance_options.connect().await.unwrap();
    for (pragma, expected) in [
        ("temp_store", 1i64),
        ("cache_size", -16384),
        ("mmap_size", 0),
        ("threads", 2),
    ] {
        let value: i64 = sqlx::query_scalar(&format!("PRAGMA {pragma}"))
            .fetch_one(&mut maintenance)
            .await
            .unwrap();
        assert_eq!(value, expected, "{pragma}");
    }
    store.build_indexes().await.unwrap();
    assert_eq!(store.writer_cache_size().await.unwrap(), -262_144);
    assert_eq!(store.telemetry.schema_active.load(Ordering::Relaxed), 0);
    assert!(store.telemetry.schema_statements.load(Ordering::Relaxed) >= 2);
    store.ensure_reorg_indexes().await.unwrap();
}

#[tokio::test]
async fn cancelled_schema_work_releases_the_writer_and_can_retry() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteStore::open(&directory.path().join("chain.db"))
            .await
            .unwrap(),
    );
    let worker = store.clone();
    let task = tokio::spawn(async move {
        worker.run_schema("CREATE TABLE cancelled_copy AS WITH RECURSIVE counter(value) AS (VALUES(0) UNION ALL SELECT value+1 FROM counter WHERE value < 1000000000) SELECT value FROM counter".into()).await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.telemetry.schema_vm_steps.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.telemetry.schema_active.load(Ordering::Relaxed) != 0 {
            tokio::task::yield_now().await;
        }
        store.build_indexes().await.unwrap();
        let batch = store.begin().await.unwrap();
        store.commit(batch).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(store.telemetry.schema_errors.load(Ordering::Relaxed), 1);
    let exists: i64 =
        sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name = 'cancelled_copy'")
            .fetch_one(&mut *store.writer.lock().await)
            .await
            .unwrap();
    assert_eq!(exists, 0);
}

#[tokio::test]
async fn failed_schema_work_preserves_the_regular_writer() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("chain.db"))
        .await
        .unwrap();
    assert!(store.run_schema("CREATE INDEX IF NOT EXISTS coin_record_confirmed_index ON coin_record(confirmed_index); CREATE INDEX bad ON missing_table(no_column)".into()).await.is_err());
    assert_eq!(store.telemetry.schema_active.load(Ordering::Relaxed), 0);
    assert_eq!(store.writer_cache_size().await.unwrap(), -262_144);
    store.build_indexes().await.unwrap();
}
