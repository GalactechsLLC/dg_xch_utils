mod block;
mod coin;

use crate::error::StoreError;
use crate::sqlite::amount_from_be;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};

pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    /// Connect to `url` (a `postgres://` URL) and apply the migrations for the enabled feature tier.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the database cannot be reached or migrated.
    pub async fn open(url: &str) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(8)
            // Asynchronous commit: COMMIT returns before the WAL fsync. Atomicity and transaction
            // ORDER are preserved — a crash loses at most a clean suffix of committed
            // transactions, never a torn or reordered state, and chain data is resyncable from
            // the surviving peak. Exception: the single-transaction reorg overrides this with
            // `SET LOCAL synchronous_commit = on` (coin.rs `rollback_to_in`) — rare, must be durable.
            .after_connect(|conn, _meta| {
                Box::pin(async move {
                    sqlx::Executor::execute(conn, "SET synchronous_commit = off").await?;
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        migrate(&pool).await?;
        Ok(Self { pool })
    }
}

async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    sqlx::raw_sql(include_str!(
        "../../migrations/postgres/0001_coin_record.sql"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(include_str!("../../migrations/postgres/0002_block.sql"))
        .execute(pool)
        .await?;
    // 0003 (service indexes) and 0006 (reorg indexes) are NOT applied at open: secondary indexes
    // on coin_record are pure write-amplification during bulk sync. The server builds them once
    // at the sync->tip transition via `BlockStore::build_indexes`.
    #[cfg(feature = "hint")]
    sqlx::raw_sql(include_str!("../../migrations/postgres/0004_hint.sql"))
        .execute(pool)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/postgres/0005_sub_epoch_segments.sql"
    ))
    .execute(pool)
    .await?;
    // Per-table autovacuum tuning is idempotent and always correct — applied at every open, not
    // deferred (a syncing table needs it the most).
    sqlx::raw_sql(include_str!(
        "../../migrations/postgres/0007_autovacuum.sql"
    ))
    .execute(pool)
    .await?;
    // coin_record fillfactor=90: reserve page headroom so the insert-once/update-once (spend)
    // stays HOT (no pkey bloat, dead versions reclaimed by HOT-prune not vacuum). Idempotent
    // ALTER, applied at every open like 0007.
    sqlx::raw_sql(include_str!(
        "../../migrations/postgres/0008_coin_record_fillfactor.sql"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

fn row_to_coin_record(row: &sqlx::postgres::PgRow) -> Result<CoinRecord, StoreError> {
    let confirmed: i64 = row.try_get("confirmed_index")?;
    let spent: i64 = row.try_get("spent_index")?;
    let coinbase: i64 = row.try_get("coinbase")?;
    let timestamp: i64 = row.try_get("timestamp")?;
    let puzzle_hash: Bytes32 = row.try_get("puzzle_hash")?;
    let coin_parent: Bytes32 = row.try_get("coin_parent")?;
    let amount: Vec<u8> = row.try_get("amount")?;
    let spent_index = spent as u32;
    Ok(CoinRecord {
        coin: Coin {
            parent_coin_info: coin_parent,
            puzzle_hash,
            amount: amount_from_be(&amount)?,
        },
        confirmed_block_index: confirmed as u32,
        spent_block_index: spent_index,
        coinbase: coinbase != 0,
        timestamp: timestamp as u64,
        spent: spent_index != 0,
    })
}
