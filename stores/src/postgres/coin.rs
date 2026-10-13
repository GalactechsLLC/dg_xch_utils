use crate::error::StoreError;
use crate::postgres::{PostgresStore, row_to_coin_record};
use crate::sqlite::amount_be;
use crate::traits::CoinStore;
#[cfg(feature = "coin-index")]
use crate::traits::{coin_state_from_record, merge_coin_states_bounded, page_coin_states};
use async_trait::async_trait;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::wallet::{CoinState, CoinStateFilters};

const SELECT_COIN: &str = "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, \
    amount, timestamp FROM coin_record WHERE coin_name = $1";

// Postgres's u16 bind-parameter budget (65,535) sets the chunk sizes: 8 binds per addition row,
// 1 + N binds per removal statement.
const ADDITIONS_PER_STATEMENT: usize = 5_000;
const REMOVALS_PER_STATEMENT: usize = 10_000;

// One block's coin deltas, parameterized over the connection: a self-contained transaction from
// `apply_block`, or joined onto an open batch from `apply_block_in` (one fsync per block).
// Multi-row statements, not per-coin queries: each coin as its own INSERT is one network
// round-trip to the database per coin.
async fn apply_block_on(
    conn: &mut sqlx::PgConnection,
    height: u32,
    timestamp: u64,
    additions: &[CoinRecord],
    removals: &[Bytes32],
) -> Result<(), StoreError> {
    // Batches are sorted by coin_name BEFORE chunking (see lib.rs sort_additions_by_name): each
    // statement's rows then probe adjacent pkey leaf pages instead of one random multi-GB btree
    // descent per coin, which dominates the confirm on high-latency storage.
    let additions = crate::sort_additions_by_name(additions);
    for chunk in additions.chunks(ADDITIONS_PER_STATEMENT) {
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO coin_record \
             (coin_name, confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, timestamp) ",
        );
        qb.push_values(chunk, |mut b, (name, cr)| {
            b.push_bind(*name)
                .push_bind(i64::from(height))
                .push_bind(0_i64)
                .push_bind(i64::from(cr.coinbase))
                .push_bind(cr.coin.puzzle_hash)
                .push_bind(cr.coin.parent_coin_info)
                .push_bind(amount_be(cr.coin.amount))
                .push_bind(timestamp as i64);
        });
        qb.push(
            " ON CONFLICT(coin_name) DO UPDATE SET confirmed_index = excluded.confirmed_index, \
             spent_index = 0, coinbase = excluded.coinbase, puzzle_hash = excluded.puzzle_hash, \
             coin_parent = excluded.coin_parent, amount = excluded.amount, timestamp = excluded.timestamp",
        );
        // persistent(false): the SQL text varies with chunk arity, so a prepared-statement cache
        // entry is created and retained per distinct arity per connection. sqlx's QueryBuilder
        // doc prescribes non-persistent execution for variable-length tuples.
        qb.build().persistent(false).execute(&mut *conn).await?;
    }
    let removals = crate::sorted_removal_names(removals);
    for chunk in removals.chunks(REMOVALS_PER_STATEMENT) {
        let mut qb = sqlx::QueryBuilder::new("UPDATE coin_record SET spent_index = ");
        qb.push_bind(i64::from(height));
        qb.push(" WHERE coin_name IN (");
        let mut sep = qb.separated(", ");
        for name in chunk {
            sep.push_bind(*name);
        }
        qb.push(")");
        // Same variable-arity cache churn as the insert above.
        qb.build().persistent(false).execute(&mut *conn).await?;
    }
    Ok(())
}

// The fork revert, parameterized over the connection: a self-contained transaction from
// `rollback_to`, or the FIRST statements of the engine's single-transaction reorg from
// `rollback_to_in` (rollback + branch re-applies + peak flip commit as one unit).
async fn rollback_to_on(
    conn: &mut sqlx::PgConnection,
    fork_height: u32,
) -> Result<u64, StoreError> {
    let deleted = sqlx::query("DELETE FROM coin_record WHERE confirmed_index > $1")
        .bind(i64::from(fork_height))
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let unspent = sqlx::query("UPDATE coin_record SET spent_index = 0 WHERE spent_index > $1")
        .bind(i64::from(fork_height))
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(deleted + unspent)
}

#[async_trait]
impl CoinStore for PostgresStore {
    #[cfg(feature = "coin-index")]
    async fn coin_page(
        &self,
        request: &dg_xch_core::protocols::explorer::CoinPageRequest,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        request
            .validate()
            .map_err(|error| StoreError::Corrupt(error.into()))?;
        let mut q = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, timestamp FROM coin_record WHERE ",
        );
        let (column, ids) = if request.parent_ids.is_empty() {
            ("puzzle_hash", &request.puzzle_hashes)
        } else {
            ("coin_parent", &request.parent_ids)
        };
        q.push(column).push(" IN (");
        {
            let mut hashes = q.separated(",");
            for hash in ids {
                hashes.push_bind(*hash);
            }
        }
        q.push(") AND confirmed_index <= ")
            .push_bind(i64::from(height));
        if let Some(spent) = request.spent {
            if spent {
                q.push(" AND spent_index > 0 AND spent_index <= ")
                    .push_bind(i64::from(height));
            } else {
                q.push(" AND (spent_index = 0 OR spent_index > ")
                    .push_bind(i64::from(height))
                    .push(")");
            }
        }
        q.push(" ORDER BY confirmed_index DESC, coin_name DESC LIMIT ")
            .push_bind(i64::from(request.limit))
            .push(" OFFSET ")
            .push_bind(i64::from(request.offset));
        let rows = q.build().fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                let mut record = row_to_coin_record(row)?;
                if record.spent_block_index > height {
                    record.spent_block_index = 0;
                    record.spent = false;
                }
                Ok(record)
            })
            .collect()
    }
    #[cfg(feature = "coin-index")]
    async fn coin_summary(
        &self,
        ph: &Bytes32,
        height: u32,
    ) -> Result<dg_xch_core::protocols::explorer::CoinSummary, StoreError> {
        let started = std::time::Instant::now();
        let mut total = crate::explorer::Summary::default();
        let mut cursor: Option<Bytes32> = None;
        // A stable read transaction prevents mixing state across keyset batches.
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *tx)
            .await?;
        loop {
            crate::explorer::work_deadline(started)?;
            let mut q = sqlx::QueryBuilder::<sqlx::Postgres>::new(
                "SELECT coin_name, confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, timestamp FROM coin_record WHERE puzzle_hash=",
            );
            q.push_bind(*ph)
                .push(" AND confirmed_index <= ")
                .push_bind(i64::from(height));
            if let Some(cursor) = cursor {
                q.push(" AND coin_name > ").push_bind(cursor);
            }
            q.push(" ORDER BY coin_name LIMIT 1000");
            let rows = q.build().fetch_all(&mut *tx).await?;
            for row in &rows {
                total.add(&row_to_coin_record(row)?, height)?;
            }
            if let Some(row) = rows.last() {
                cursor = Some(sqlx::Row::try_get(row, "coin_name")?);
            }
            if rows.len() < 1000 {
                break;
            }
        }
        crate::explorer::work_deadline(started)?;
        tx.commit().await?;
        Ok(total.finish())
    }

    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        let row = sqlx::query(SELECT_COIN)
            .bind(*coin_name)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(row_to_coin_record).transpose()
    }

    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (1..=names.len())
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE coin_name IN ({placeholders})"
        );
        // Variable-arity IN list — same non-persistent rule as apply_block_on.
        let mut query = sqlx::query(&sql).persistent(false);
        for name in names {
            query = query.bind(*name);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        apply_block_on(&mut tx, height, timestamp, additions, removals).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn apply_block_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        apply_block_on(batch.pg_conn()?, height, timestamp, additions, removals).await
    }

    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let reverted = rollback_to_on(&mut tx, fork_height).await?;
        tx.commit().await?;
        Ok(reverted)
    }

    async fn rollback_to_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError> {
        let conn = batch.pg_conn()?;
        // The reorg transaction commits SYNCHRONOUSLY. The pool-wide `synchronous_commit = off`
        // (mod.rs) is fine for resyncable per-block applies, but losing an acknowledged reorg
        // after a crash silently resurrects the abandoned branch until some later commit lands.
        // `SET LOCAL` scopes the override to THIS transaction only.
        sqlx::query("SET LOCAL synchronous_commit = on")
            .execute(&mut *conn)
            .await?;
        rollback_to_on(conn, fork_height).await
    }

    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError> {
        // Built NON-concurrently: the caller is the reorg itself (no live coin applies, so the
        // brief SHARE lock is free), and a CONCURRENTLY build interrupted by a crash leaves an
        // INVALID index that `IF NOT EXISTS` then skips forever. Same index NAMES as migration
        // 0006, so either path is a no-op after the other. BRIN on confirmed_index
        // (block-sequential, heap-correlated); btree on spent_index (not heap-correlated).
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS coin_record_confirmed_index ON coin_record USING BRIN (confirmed_index)",
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS coin_record_spent_index ON coin_record (spent_index)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[cfg(feature = "coin-index")]
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = $1 AND spent_index = 0",
        )
        .bind(*ph)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE coin_parent = $1",
        )
        .bind(*parent)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "coin-index")]
    async fn get_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        include_spent: bool,
        max_items: usize,
    ) -> Result<Vec<CoinState>, StoreError> {
        if puzzle_hashes.is_empty() {
            return Ok(Vec::new());
        }
        // Per-puzzle-hash indexed query with a running LIMIT budget so the reply stays bounded by
        // the caller's `max_items`.
        let spent_clause = if include_spent {
            ""
        } else {
            " AND spent_index <= 0"
        };
        let sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = $1 \
             AND (confirmed_index >= $2 OR spent_index >= $2){spent_clause} LIMIT $3"
        );
        let mut out = Vec::new();
        for ph in puzzle_hashes {
            if out.len() >= max_items {
                break;
            }
            let remaining = i64::try_from(max_items - out.len()).unwrap_or(i64::MAX);
            let rows = sqlx::query(&sql)
                .bind(*ph)
                .bind(i64::from(min_height))
                .bind(remaining)
                .fetch_all(&self.pool)
                .await?;
            for r in &rows {
                out.push(coin_state_from_record(&row_to_coin_record(r)?));
            }
        }
        Ok(out)
    }

    #[cfg(feature = "coin-index")]
    async fn batch_coin_states_by_puzzle_hashes(
        &self,
        puzzle_hashes: &[Bytes32],
        min_height: u32,
        filters: &CoinStateFilters,
        max_items: usize,
    ) -> Result<(Vec<CoinState>, Option<u32>), StoreError> {
        // Nothing requested, or filters that admit nothing, finish empty.
        if puzzle_hashes.is_empty() || (!filters.include_spent && !filters.include_unspent) {
            return Ok((Vec::new(), None));
        }
        // Same shape as the SQLite leg (see sqlite/coin.rs): per-hash indexed probes, each
        // height-ordered (GREATEST is Postgres's scalar two-arg MAX) and LIMITed to max_items + 1
        // in SQL, merged bounded. The amount predicate compares the 8-byte big-endian BYTEA
        // bytewise, which IS numeric >=; the zero blob is a no-op.
        let height_filter = match (filters.include_spent, filters.include_unspent) {
            (true, true) => "",
            (true, false) => " AND spent_index > 0",
            (false, true) => " AND spent_index <= 0",
            (false, false) => unreachable!("handled above"),
        };
        let sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = $1 \
             AND (confirmed_index >= $2 OR spent_index >= $2){height_filter} AND amount >= $3 \
             ORDER BY GREATEST(confirmed_index, spent_index) ASC LIMIT $4"
        );
        #[cfg(feature = "hint")]
        let hint_sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE coin_name IN \
             (SELECT coin_name FROM coin_hint WHERE hint = $1) \
             AND (confirmed_index >= $2 OR spent_index >= $2){height_filter} AND amount >= $3 \
             ORDER BY GREATEST(confirmed_index, spent_index) ASC LIMIT $4"
        );
        let limit = i64::try_from(max_items.saturating_add(1)).unwrap_or(i64::MAX);
        let min_amount = amount_be(filters.min_amount);
        let mut merged = std::collections::HashMap::new();
        for ph in puzzle_hashes {
            let rows = sqlx::query(&sql)
                .bind(*ph)
                .bind(i64::from(min_height))
                .bind(min_amount.clone())
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
            let states = rows
                .iter()
                .map(|r| Ok(coin_state_from_record(&row_to_coin_record(r)?)))
                .collect::<Result<Vec<_>, StoreError>>()?;
            merge_coin_states_bounded(&mut merged, states, max_items + 1);
            #[cfg(feature = "hint")]
            if filters.include_hinted {
                let rows = sqlx::query(&hint_sql)
                    .bind(*ph)
                    .bind(i64::from(min_height))
                    .bind(min_amount.clone())
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?;
                let states = rows
                    .iter()
                    .map(|r| Ok(coin_state_from_record(&row_to_coin_record(r)?)))
                    .collect::<Result<Vec<_>, StoreError>>()?;
                merge_coin_states_bounded(&mut merged, states, max_items + 1);
            }
        }
        Ok(page_coin_states(merged, max_items))
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE confirmed_index = $1",
        )
        .bind(i64::from(height))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_removed_at_height(
        &self,
        height: u32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE spent_index = $1",
        )
        .bind(i64::from(height))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        use sqlx::Row;
        // LIMIT in the query — never fetch-then-truncate.
        let rows = sqlx::query("SELECT coin_name FROM coin_hint WHERE hint = $1 LIMIT $2")
            .bind(*hint)
            .bind(i64::try_from(max_items).unwrap_or(i64::MAX))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<Bytes32, _>("coin_name")?))
            .collect()
    }

    async fn apply_hints_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        pairs: &[(Bytes32, Bytes32)],
    ) -> Result<(), StoreError> {
        #[cfg(feature = "hint")]
        {
            apply_hints_on(batch.pg_conn()?, pairs).await
        }
        #[cfg(not(feature = "hint"))]
        {
            let _ = (batch, pairs);
            Ok(())
        }
    }

    async fn apply_hints(&self, pairs: &[(Bytes32, Bytes32)]) -> Result<(), StoreError> {
        #[cfg(feature = "hint")]
        {
            let mut tx = self.pool.begin().await?;
            apply_hints_on(&mut tx, pairs).await?;
            tx.commit().await?;
            Ok(())
        }
        #[cfg(not(feature = "hint"))]
        {
            let _ = pairs;
            Ok(())
        }
    }
}

// One block's create-coin hints, parameterized over the connection: joined onto the block's open
// batch from `apply_hints_in`, or its own transaction from `apply_hints`. `ON CONFLICT DO NOTHING`
// keeps re-apply/replay idempotent against the `(hint, coin_name)` primary key.
#[cfg(feature = "hint")]
async fn apply_hints_on(
    conn: &mut sqlx::PgConnection,
    pairs: &[(Bytes32, Bytes32)],
) -> Result<(), StoreError> {
    for (hint, coin_name) in pairs {
        sqlx::query(
            "INSERT INTO coin_hint (hint, coin_name) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(*hint)
        .bind(*coin_name)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}
