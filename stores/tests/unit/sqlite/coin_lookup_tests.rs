use super::{COIN_LOOKUP_BATCH, coin_lookup_query};
use crate::sqlite::SqliteStore;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use sqlx::{Connection, Row};

#[tokio::test]
async fn batched_lookup_uses_primary_key_with_and_without_statistics() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("plan.sqlite"))
        .await
        .unwrap();
    let mut writer = store.writer.lock().await;
    for analyze in [false, true] {
        if analyze {
            sqlx::query(
                "WITH RECURSIVE sequence(value) AS (VALUES(1) UNION ALL \
                 SELECT value + 1 FROM sequence WHERE value < 1000) \
                 INSERT INTO coin_record (coin_name, confirmed_index, spent_index, coinbase, \
                 puzzle_hash, coin_parent, amount, timestamp) \
                 SELECT randomblob(32), 1, 0, 0, zeroblob(32), zeroblob(32), zeroblob(8), 1 FROM sequence",
            )
            .execute(&mut *writer)
            .await
            .unwrap();
            sqlx::query("ANALYZE").execute(&mut *writer).await.unwrap();
        }
        for size in [1, 17, COIN_LOOKUP_BATCH] {
            let names = vec![Bytes32::from([7; 32]); size];
            let query = coin_lookup_query(&names);
            let sql = format!("EXPLAIN QUERY PLAN {}", query.sql());
            let mut explain = sqlx::query(&sql);
            for (ordinal, name) in names.iter().enumerate() {
                explain = explain.bind(ordinal as i64).bind(*name);
            }
            let plan: Vec<String> = explain
                .fetch_all(&mut *writer)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.get("detail"))
                .collect();
            assert!(
                plan.iter()
                    .any(|detail| detail.contains("SEARCH coins USING PRIMARY KEY")),
                "{plan:?}"
            );
            assert!(
                !plan.iter().any(|detail| detail.contains("SCAN coins")),
                "{plan:?}"
            );
        }
    }
}

#[tokio::test]
async fn memory_statement_journal_rolls_back_only_the_failed_statement() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("statement.sqlite"))
        .await
        .unwrap();
    let mut writer = store.writer.lock().await;
    sqlx::query("CREATE TABLE statement_atomicity (id INTEGER PRIMARY KEY, payload BLOB NOT NULL)")
        .execute(&mut *writer)
        .await
        .unwrap();
    sqlx::query(
        "WITH RECURSIVE sequence(value) AS (VALUES(1) UNION ALL \
         SELECT value + 1 FROM sequence WHERE value < 512) \
         INSERT INTO statement_atomicity SELECT value, zeroblob(4096) FROM sequence",
    )
    .execute(&mut *writer)
    .await
    .unwrap();
    let mut transaction = writer.begin().await.unwrap();
    sqlx::query("INSERT INTO statement_atomicity VALUES (513, zeroblob(16))")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let result = sqlx::query(
        "UPDATE statement_atomicity SET payload = \
         CASE WHEN id = 512 THEN NULL ELSE zeroblob(8192) END WHERE id <= 512",
    )
    .execute(&mut *transaction)
    .await;
    assert!(result.is_err());
    let lengths: i64 = sqlx::query_scalar("SELECT sum(length(payload)) FROM statement_atomicity")
        .fetch_one(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(lengths, 512 * 4096 + 16);
    transaction.commit().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM statement_atomicity")
        .fetch_one(&mut *writer)
        .await
        .unwrap();
    assert_eq!(count, 513);
}

#[tokio::test]
async fn sqlite_connections_use_memory_temp_storage_and_keep_wal_normal() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&directory.path().join("temp.sqlite"))
        .await
        .unwrap();
    let mut writer = store.writer.lock().await;
    let mut reader = store.read.acquire().await.unwrap();
    for connection in [&mut *writer, &mut *reader] {
        let temp_store: i64 = sqlx::query_scalar("PRAGMA temp_store")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(temp_store, 2);
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(synchronous, 1);
    }
}
