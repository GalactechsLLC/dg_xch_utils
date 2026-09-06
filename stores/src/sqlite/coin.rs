use crate::error::StoreError;
use crate::sqlite::{SqliteStore, amount_be, row_to_coin_record};
use crate::traits::CoinStore;
#[cfg(feature = "coin-index")]
use crate::traits::{coin_state_from_record, merge_coin_states_bounded, page_coin_states};
use async_trait::async_trait;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
#[cfg(feature = "coin-index")]
use dg_xch_core::protocols::wallet::{CoinState, CoinStateFilters};
use dg_xch_core::traits::SizedBytes;
use sqlx::Connection;
use std::sync::atomic::Ordering;

const SELECT_COIN: &str = "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, \
    amount, timestamp FROM coin_record WHERE coin_name = ?";

const COIN_LOOKUP_BATCH: usize = 256;

#[cfg(test)]
#[path = "../../tests/unit/sqlite/coin_lookup_tests.rs"]
mod coin_lookup_tests;

fn coin_lookup_query(names: &[Bytes32]) -> sqlx::QueryBuilder<'_, sqlx::Sqlite> {
    let mut query = sqlx::QueryBuilder::new("WITH requested(ordinal, coin_name) AS (");
    query.push_values(names.iter().enumerate(), |mut bindings, (ordinal, name)| {
        bindings.push_bind(ordinal as i64).push_bind(*name);
    });
    query.push(
        ") SELECT coins.confirmed_index, coins.spent_index, coins.coinbase, \
         coins.puzzle_hash, coins.coin_parent, coins.amount, coins.timestamp \
         FROM requested CROSS JOIN coin_record AS coins \
         WHERE coins.coin_name = requested.coin_name ORDER BY requested.ordinal",
    );
    query
}

async fn write_additions(
    conn: &mut sqlx::SqliteConnection,
    additions: &[(Bytes32, CoinRecord)],
    telemetry: &crate::telemetry::StoreTelemetry,
) -> Result<(), StoreError> {
    let _timer = telemetry.coin_additions.start();
    for chunk in additions.chunks(100) {
        let mut query = sqlx::QueryBuilder::new(
            "INSERT OR REPLACE INTO coin_record (coin_name, confirmed_index, spent_index, \
             coinbase, puzzle_hash, coin_parent, amount, timestamp) ",
        );
        query.push_values(chunk, |mut bindings, (name, record)| {
            bindings
                .push_bind(*name)
                .push_bind(i64::from(record.confirmed_block_index))
                .push_bind(i64::from(record.spent_block_index))
                .push_bind(i64::from(record.coinbase))
                .push_bind(record.coin.puzzle_hash)
                .push_bind(record.coin.parent_coin_info)
                .push_bind(amount_be(record.coin.amount))
                .push_bind(record.timestamp as i64);
        });
        query
            .build()
            .persistent(chunk.len() == 100)
            .execute(&mut *conn)
            .await?;
        telemetry.coin_additions.statement(chunk.len());
    }
    Ok(())
}

async fn write_removals(
    conn: &mut sqlx::SqliteConnection,
    removals: &[(Bytes32, u32)],
    telemetry: &crate::telemetry::StoreTelemetry,
) -> Result<(), StoreError> {
    let _timer = telemetry.coin_removals.start();
    for chunk in removals.chunks(100) {
        let mut query = sqlx::QueryBuilder::new("WITH changes(coin_name, height) AS (");
        query.push_values(chunk, |mut bindings, (name, height)| {
            bindings.push_bind(*name).push_bind(i64::from(*height));
        });
        query.push(
            ") UPDATE coin_record SET spent_index = changes.height \
                    FROM changes WHERE coin_record.coin_name = changes.coin_name",
        );
        query
            .build()
            .persistent(chunk.len() == 100)
            .execute(&mut *conn)
            .await?;
        telemetry.coin_removals.statement(chunk.len());
    }
    Ok(())
}

async fn apply_block_on(
    conn: &mut sqlx::SqliteConnection,
    height: u32,
    timestamp: u64,
    additions: &[CoinRecord],
    removals: &[Bytes32],
    telemetry: &crate::telemetry::StoreTelemetry,
) -> Result<(), StoreError> {
    let additions: Vec<_> = crate::sort_additions_by_name(additions)
        .into_iter()
        .map(|(name, record)| {
            (
                name,
                CoinRecord {
                    confirmed_block_index: height,
                    spent_block_index: 0,
                    spent: false,
                    timestamp,
                    ..record.clone()
                },
            )
        })
        .collect();
    write_additions(conn, &additions, telemetry).await?;
    let removals: Vec<_> = crate::sorted_removal_names(removals)
        .into_iter()
        .map(|name| (name, height))
        .collect();
    write_removals(conn, &removals, telemetry).await
}

// The fork revert, parameterized over the connection: a self-contained transaction from
// `rollback_to`, or the FIRST statements of the engine's single-transaction reorg from
// `rollback_to_in`, where rollback + branch re-applies + peak flip commit as one unit.
async fn rollback_to_on(
    conn: &mut sqlx::SqliteConnection,
    fork_height: u32,
) -> Result<u64, StoreError> {
    let deleted = sqlx::query("DELETE FROM coin_record WHERE confirmed_index > ?")
        .bind(i64::from(fork_height))
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let unspent = sqlx::query("UPDATE coin_record SET spent_index = 0 WHERE spent_index > ?")
        .bind(i64::from(fork_height))
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(deleted + unspent)
}

fn prepare_coin_changes(
    changes: &[crate::types::CoinChanges<'_>],
) -> crate::types::PreparedCoinWindow {
    let mut additions = std::collections::HashMap::new();
    let mut removals = std::collections::HashMap::new();
    for change in changes {
        for record in change.additions {
            let name = record.coin.name();
            removals.remove(&name);
            additions.insert(
                name,
                CoinRecord {
                    confirmed_block_index: change.height,
                    spent_block_index: 0,
                    spent: false,
                    timestamp: change.timestamp,
                    ..record.clone()
                },
            );
        }
        for name in change.removals {
            if let Some(record) = additions.get_mut(name) {
                record.spent_block_index = change.height;
                record.spent = true;
            } else {
                removals.insert(*name, change.height);
            }
        }
    }
    let mut additions: Vec<_> = additions.into_iter().collect();
    let mut removals: Vec<_> = removals.into_iter().collect();
    additions.sort_unstable_by_key(|(name, _)| name.bytes());
    removals.sort_unstable_by_key(|(name, _)| name.bytes());

    #[cfg(feature = "hint")]
    let hints = {
        let mut hints: Vec<_> = changes
            .iter()
            .flat_map(|change| change.hints.iter().copied())
            .collect();
        hints.sort_unstable_by_key(|(hint, name)| (hint.bytes(), name.bytes()));
        hints.dedup();
        hints
    };
    #[cfg(not(feature = "hint"))]
    let hints = Vec::new();
    crate::types::PreparedCoinWindow::Sqlite {
        additions,
        removals,
        hints,
    }
}

#[async_trait]
impl CoinStore for SqliteStore {
    async fn prepare_coin_window(
        &self,
        changes: Vec<crate::types::OwnedCoinChanges>,
    ) -> Result<crate::types::PreparedCoinWindow, StoreError> {
        let telemetry = self.telemetry.clone();
        tokio::task::spawn_blocking(move || {
            let _timer = telemetry.coin_prepare.start();
            let input_rows: usize = changes
                .iter()
                .map(|change| {
                    change.additions.len()
                        + change.removals.len()
                        + if cfg!(feature = "hint") {
                            change.hints.len()
                        } else {
                            0
                        }
                })
                .sum();
            let changes: Vec<_> = changes
                .iter()
                .map(crate::types::OwnedCoinChanges::borrowed)
                .collect();
            let prepared =
                dg_xch_core::compute::map(dg_xch_core::compute::Phase::CoinPrepare, &[()], |_| {
                    prepare_coin_changes(&changes)
                })
                .pop()
                .expect("one coin preparation job");
            if let crate::types::PreparedCoinWindow::Sqlite {
                additions,
                removals,
                hints,
            } = &prepared
            {
                telemetry
                    .coin_prepare_input_rows
                    .fetch_add(input_rows as u64, Ordering::Relaxed);
                telemetry.coin_prepare_output_rows.fetch_add(
                    (additions.len() + removals.len() + hints.len()) as u64,
                    Ordering::Relaxed,
                );
            }
            prepared
        })
        .await
        .map_err(|error| StoreError::Batch(format!("coin preparation: {error}")))
    }

    async fn apply_prepared_coin_window_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        prepared: crate::types::PreparedCoinWindow,
    ) -> Result<(), StoreError> {
        let crate::types::PreparedCoinWindow::Sqlite {
            additions,
            removals,
            hints: _hints,
        } = prepared
        else {
            return Err(StoreError::Batch(
                "coins prepared by another backend".into(),
            ));
        };
        let conn = batch.sqlite_conn()?;
        write_additions(conn, &additions, &self.telemetry).await?;
        write_removals(conn, &removals, &self.telemetry).await?;
        #[cfg(feature = "hint")]
        write_hints_on(conn, &_hints, &self.telemetry).await?;
        Ok(())
    }

    async fn apply_coin_window_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        changes: &[crate::types::CoinChanges<'_>],
    ) -> Result<(), StoreError> {
        self.apply_prepared_coin_window_in(batch, prepare_coin_changes(changes))
            .await
    }

    async fn get_coin_record(&self, coin_name: &Bytes32) -> Result<Option<CoinRecord>, StoreError> {
        let _timer = self.telemetry.coin_lookup.start();
        self.telemetry.coin_reads.fetch_add(1, Ordering::Relaxed);
        let row = sqlx::query(SELECT_COIN)
            .bind(*coin_name)
            .fetch_optional(&self.read)
            .await?;
        self.telemetry.coin_lookup.statement(1);
        row.as_ref().map(row_to_coin_record).transpose()
    }

    async fn get_coin_records(&self, names: &[Bytes32]) -> Result<Vec<CoinRecord>, StoreError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        if names.len() == 1 {
            return Ok(self.get_coin_record(&names[0]).await?.into_iter().collect());
        }
        let _timer = self.telemetry.coin_lookup.start();
        self.telemetry
            .coin_reads
            .fetch_add(names.len() as u64, Ordering::Relaxed);
        let mut conn = self.read.acquire().await?;
        let mut out = Vec::with_capacity(names.len());
        for chunk in names.chunks(COIN_LOOKUP_BATCH) {
            let rows = coin_lookup_query(chunk)
                .build()
                .persistent(chunk.len() == COIN_LOOKUP_BATCH)
                .fetch_all(&mut *conn)
                .await?;
            self.telemetry.coin_lookup.statement(chunk.len());
            for row in rows {
                out.push(row_to_coin_record(&row)?);
            }
        }
        Ok(out)
    }

    async fn apply_block(
        &self,
        height: u32,
        timestamp: u64,
        additions: &[CoinRecord],
        removals: &[Bytes32],
    ) -> Result<(), StoreError> {
        let mut guard = self.writer.lock().await;
        let mut tx = guard.begin().await?;
        apply_block_on(
            &mut tx,
            height,
            timestamp,
            additions,
            removals,
            &self.telemetry,
        )
        .await?;
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
        apply_block_on(
            batch.sqlite_conn()?,
            height,
            timestamp,
            additions,
            removals,
            &self.telemetry,
        )
        .await
    }

    async fn rollback_to(&self, fork_height: u32) -> Result<u64, StoreError> {
        let mut guard = self.writer.lock().await;
        let mut tx = guard.begin().await?;
        let reverted = rollback_to_on(&mut tx, fork_height).await?;
        tx.commit().await?;
        Ok(reverted)
    }

    async fn rollback_to_in(
        &self,
        batch: &mut crate::types::BatchHandle,
        fork_height: u32,
    ) -> Result<u64, StoreError> {
        rollback_to_on(batch.sqlite_conn()?, fork_height).await
    }

    async fn ensure_reorg_indexes(&self) -> Result<(), StoreError> {
        for stmt in [
            "CREATE INDEX IF NOT EXISTS coin_record_confirmed_index ON coin_record (confirmed_index)",
            "CREATE INDEX IF NOT EXISTS coin_record_spent_index ON coin_record (spent_index)",
        ] {
            let mut guard = self.writer.lock().await;
            sqlx::query(stmt).execute(&mut *guard).await?;
        }
        Ok(())
    }

    #[cfg(feature = "coin-index")]
    async fn get_unspent_by_puzzle_hash(
        &self,
        ph: &Bytes32,
    ) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = ? AND spent_index = 0",
        )
        .bind(*ph)
        .fetch_all(&self.read)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_by_parent(&self, parent: &Bytes32) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE coin_parent = ?",
        )
        .bind(*parent)
        .fetch_all(&self.read)
        .await?;
        rows.iter().map(row_to_coin_record).collect()
    }

    #[cfg(feature = "coin-index")]
    async fn get_coins_added_at_height(&self, height: u32) -> Result<Vec<CoinRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE confirmed_index = ?",
        )
        .bind(i64::from(height))
        .fetch_all(&self.read)
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
             timestamp FROM coin_record WHERE spent_index = ?",
        )
        .bind(i64::from(height))
        .fetch_all(&self.read)
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
        // Per-puzzle-hash point query over the `coin_puzzle_hash` index (the same scan-avoidance
        // discipline as get_coin_records), with a running LIMIT budget so the whole reply stays bounded
        // by the caller's `max_items`, decrementing the LIMIT per batch.
        let spent_clause = if include_spent {
            ""
        } else {
            " AND spent_index <= 0"
        };
        let sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = ? \
             AND (confirmed_index >= ? OR spent_index >= ?){spent_clause} LIMIT ?"
        );
        let mut conn = self.read.acquire().await?;
        let mut out = Vec::new();
        for ph in puzzle_hashes {
            if out.len() >= max_items {
                break;
            }
            let remaining = i64::try_from(max_items - out.len()).unwrap_or(i64::MAX);
            let rows = sqlx::query(&sql)
                .bind(*ph)
                .bind(i64::from(min_height))
                .bind(i64::from(min_height))
                .bind(remaining)
                .fetch_all(&mut *conn)
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
        // Nothing requested, or filters that admit nothing, finishes empty.
        if puzzle_hashes.is_empty() || (!filters.include_spent && !filters.include_unspent) {
            return Ok((Vec::new(), None));
        }
        // The spent/unspent predicates and the >= min_amount filter. Amount is stored as an
        // 8-byte big-endian blob, so a bytewise >= IS a numeric >=, and the zero blob makes the
        // predicate a no-op.
        let height_filter = match (filters.include_spent, filters.include_unspent) {
            (true, true) => "",
            (true, false) => " AND spent_index > 0",
            (false, true) => " AND spent_index <= 0",
            (false, false) => unreachable!("handled above"),
        };
        // Per-puzzle-hash point probes over the coin_puzzle_hash index (the same scan-avoidance
        // discipline as get_coin_records: a long dynamic IN-list collapses the planner to a full
        // scan). Each probe is ORDERED by activity height and LIMITed to max_items + 1 IN SQL,
        // with the bounded merge keeping only the smallest max_items + 1 overall.
        let sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE puzzle_hash = ? \
             AND (confirmed_index >= ? OR spent_index >= ?){height_filter} AND amount >= ? \
             ORDER BY MAX(confirmed_index, spent_index) ASC LIMIT ?"
        );
        // The hint join: coins whose HINT is one of the requested hashes — the CAT/NFT discovery
        // read. Same ordering + limit + filters.
        #[cfg(feature = "hint")]
        let hint_sql = format!(
            "SELECT confirmed_index, spent_index, coinbase, puzzle_hash, coin_parent, amount, \
             timestamp FROM coin_record WHERE coin_name IN \
             (SELECT coin_name FROM coin_hint WHERE hint = ?) \
             AND (confirmed_index >= ? OR spent_index >= ?){height_filter} AND amount >= ? \
             ORDER BY MAX(confirmed_index, spent_index) ASC LIMIT ?"
        );
        let limit = i64::try_from(max_items.saturating_add(1)).unwrap_or(i64::MAX);
        let min_amount = amount_be(filters.min_amount);
        let mut conn = self.read.acquire().await?;
        let mut merged = std::collections::HashMap::new();
        for ph in puzzle_hashes {
            let rows = sqlx::query(&sql)
                .bind(*ph)
                .bind(i64::from(min_height))
                .bind(i64::from(min_height))
                .bind(min_amount.clone())
                .bind(limit)
                .fetch_all(&mut *conn)
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
                    .bind(i64::from(min_height))
                    .bind(min_amount.clone())
                    .bind(limit)
                    .fetch_all(&mut *conn)
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

    #[cfg(feature = "hint")]
    async fn get_coins_for_hint(
        &self,
        hint: &Bytes32,
        max_items: usize,
    ) -> Result<Vec<Bytes32>, StoreError> {
        use sqlx::Row;
        // LIMIT in the query — never fetch-then-truncate.
        let rows = sqlx::query("SELECT coin_name FROM coin_hint WHERE hint = ? LIMIT ?")
            .bind(*hint)
            .bind(i64::try_from(max_items).unwrap_or(i64::MAX))
            .fetch_all(&self.read)
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
            apply_hints_on(batch.sqlite_conn()?, pairs, &self.telemetry).await
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
            let mut guard = self.writer.lock().await;
            let mut tx = guard.begin().await?;
            apply_hints_on(&mut tx, pairs, &self.telemetry).await?;
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
// batch from `apply_hints_in`, or its own transaction from `apply_hints`. `INSERT OR IGNORE` keeps
// re-apply/replay idempotent against the `(hint, coin_name)` primary key.
#[cfg(feature = "hint")]
async fn apply_hints_on(
    conn: &mut sqlx::SqliteConnection,
    pairs: &[(Bytes32, Bytes32)],
    telemetry: &crate::telemetry::StoreTelemetry,
) -> Result<(), StoreError> {
    let mut pairs = pairs.to_vec();
    pairs.sort_unstable_by_key(|(hint, name)| (hint.bytes(), name.bytes()));
    pairs.dedup();
    write_hints_on(conn, &pairs, telemetry).await
}

#[cfg(feature = "hint")]
async fn write_hints_on(
    conn: &mut sqlx::SqliteConnection,
    pairs: &[(Bytes32, Bytes32)],
    telemetry: &crate::telemetry::StoreTelemetry,
) -> Result<(), StoreError> {
    let _timer = telemetry.hints.start();
    for chunk in pairs.chunks(400) {
        let mut query =
            sqlx::QueryBuilder::new("INSERT OR IGNORE INTO coin_hint (hint, coin_name) ");
        query.push_values(chunk, |mut bindings, (hint, name)| {
            bindings.push_bind(*hint).push_bind(*name);
        });
        query
            .build()
            .persistent(chunk.len() == 400)
            .execute(&mut *conn)
            .await?;
        telemetry.hints.statement(chunk.len());
    }
    Ok(())
}
