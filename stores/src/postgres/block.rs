use crate::error::StoreError;
use crate::postgres::PostgresStore;
use crate::traits::BlockStore;
use crate::types::{BatchHandle, BatchInner, BlockStatus, Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use sqlx::Row;
use std::io::Cursor;

const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

const UPSERT_RECORD: &str = "INSERT INTO block_record \
    (header_hash, prev_hash, height, weight, total_iters, is_transaction_block, sub_epoch_summary, record) \
    VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
    ON CONFLICT(header_hash) DO UPDATE SET prev_hash = excluded.prev_hash, height = excluded.height, \
    weight = excluded.weight, total_iters = excluded.total_iters, \
    is_transaction_block = excluded.is_transaction_block, sub_epoch_summary = excluded.sub_epoch_summary, \
    record = excluded.record";

// Legacy-tolerant: older blobs decode via the fallback walk; see record_compat.rs.
use crate::record_compat::decode_record;

fn wrong_backend() -> StoreError {
    StoreError::Corrupt("batch was opened by a different backend".to_string())
}

// Shared write bodies, parameterized over the connection so the same statements run either in a
// self-contained transaction (the standalone trait methods) or joined onto an open batch (the `_in`
// variants — the one-fsync-per-block apply path).
async fn upsert_records(
    conn: &mut sqlx::PgConnection,
    records: &[BlockRecord],
) -> Result<(), StoreError> {
    for r in records {
        let ses = r
            .sub_epoch_summary_included
            .as_ref()
            .map(|s| s.to_bytes(VERSION))
            .transpose()?;
        sqlx::query(UPSERT_RECORD)
            .bind(r.header_hash)
            .bind(r.prev_hash)
            .bind(i64::from(r.height))
            .bind(r.weight.to_be_bytes().to_vec())
            .bind(r.total_iters.to_be_bytes().to_vec())
            .bind(i64::from(r.is_transaction_block()))
            .bind(ses)
            .bind(r.to_bytes(VERSION)?)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

async fn set_status_on(
    conn: &mut sqlx::PgConnection,
    hh: &Bytes32,
    s: BlockStatus,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE block_record SET status = $1 WHERE header_hash = $2")
        .bind(i64::from(s.as_u8()))
        .bind(*hh)
        .execute(conn)
        .await?;
    Ok(())
}

async fn set_peak_on(conn: &mut sqlx::PgConnection, new_peak: &Bytes32) -> Result<u64, StoreError> {
    let Some(row) = sqlx::query("SELECT height FROM block_record WHERE header_hash = $1")
        .bind(*new_peak)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Err(StoreError::Corrupt(
            "set_peak: unknown header hash".to_string(),
        ));
    };
    let new_height: i64 = row.try_get("height")?;
    let mut fork_height = -1i64;
    let mut cursor = *new_peak;
    loop {
        let Some(r) = sqlx::query(
            "SELECT prev_hash, height, in_main_chain FROM block_record WHERE header_hash = $1",
        )
        .bind(cursor)
        .fetch_optional(&mut *conn)
        .await?
        else {
            break;
        };
        if r.try_get::<i64, _>("in_main_chain")? != 0 {
            fork_height = r.try_get("height")?;
            break;
        }
        cursor = r.try_get("prev_hash")?;
    }
    sqlx::query(
        "UPDATE block_record SET in_main_chain = 0 WHERE in_main_chain = 1 AND height > $1",
    )
    .bind(fork_height)
    .execute(&mut *conn)
    .await?;
    let mut links = 0u64;
    let mut cursor = *new_peak;
    loop {
        let Some(r) =
            sqlx::query("SELECT prev_hash, in_main_chain FROM block_record WHERE header_hash = $1")
                .bind(cursor)
                .fetch_optional(&mut *conn)
                .await?
        else {
            break;
        };
        if r.try_get::<i64, _>("in_main_chain")? != 0 {
            break;
        }
        let prev: Bytes32 = r.try_get("prev_hash")?;
        sqlx::query("UPDATE block_record SET in_main_chain = 1 WHERE header_hash = $1")
            .bind(cursor)
            .execute(&mut *conn)
            .await?;
        links += 1;
        cursor = prev;
    }
    sqlx::query(
        "INSERT INTO current_peak (id, header_hash, height) VALUES (0, $1, $2) \
         ON CONFLICT(id) DO UPDATE SET header_hash = excluded.header_hash, height = excluded.height",
    )
    .bind(*new_peak)
    .bind(new_height)
    .execute(&mut *conn)
    .await?;
    Ok(links)
}

async fn extend_peak_on(
    conn: &mut sqlx::PgConnection,
    extension: &[Bytes32],
    new_height: u32,
) -> Result<u64, StoreError> {
    let Some(new_peak) = extension.last() else {
        return Ok(0);
    };
    let mut links = 0u64;
    for chunk in extension.chunks(10_000) {
        let mut qb = sqlx::QueryBuilder::new(
            "UPDATE block_record SET in_main_chain = 1 WHERE header_hash IN (",
        );
        let mut separated = qb.separated(", ");
        for hash in chunk {
            separated.push_bind(*hash);
        }
        qb.push(")");
        links += qb
            .build()
            .persistent(false)
            .execute(&mut *conn)
            .await?
            .rows_affected();
    }
    if links != extension.len() as u64 {
        return Err(StoreError::Corrupt(format!(
            "extend_peak: expected {} validated records, updated {links}",
            extension.len()
        )));
    }
    sqlx::query(
        "INSERT INTO current_peak (id, header_hash, height) VALUES (0, $1, $2) \
         ON CONFLICT(id) DO UPDATE SET header_hash = excluded.header_hash, height = excluded.height",
    )
    .bind(*new_peak)
    .bind(i64::from(new_height))
    .execute(conn)
    .await?;
    Ok(links)
}

#[async_trait]
impl BlockStore for PostgresStore {
    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        let row = sqlx::query("SELECT record FROM block_record WHERE header_hash = $1")
            .bind(*hh)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(Some(decode_record(&r.try_get::<Vec<u8>, _>("record")?)?)),
            None => Ok(None),
        }
    }

    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        let row =
            sqlx::query("SELECT record FROM block_record WHERE height = $1 AND in_main_chain = 1")
                .bind(i64::from(h))
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(r) => Ok(Some(decode_record(&r.try_get::<Vec<u8>, _>("record")?)?)),
            None => Ok(None),
        }
    }

    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        let row = sqlx::query("SELECT header_hash, height FROM current_peak WHERE id = 0")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let hh: Bytes32 = r.try_get("header_hash")?;
                let h: i64 = r.try_get("height")?;
                Ok(Some((hh, h as u32)))
            }
            None => Ok(None),
        }
    }

    async fn min_record_height(&self) -> Result<Option<u32>, StoreError> {
        // First entry of the partial `block_record_height_main` index — an index-only min, cheap
        // enough for every /metrics scrape.
        let row = sqlx::query("SELECT MIN(height) AS h FROM block_record WHERE in_main_chain = 1")
            .fetch_one(&self.pool)
            .await?;
        let h: Option<i64> = row.try_get("h")?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(h.map(|v| v as u32))
    }

    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        let row = sqlx::query("SELECT body FROM block_body WHERE header_hash = $1")
            .bind(*hh)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => {
                let body: Vec<u8> = r.try_get("body")?;
                let raw = zstd::decode_all(&body[..])?;
                let block = FullBlock::from_bytes(&mut Cursor::new(&raw[..]), VERSION)?;
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    async fn get_generator_at_height(
        &self,
        h: u32,
    ) -> Result<Option<SerializedProgram>, StoreError> {
        let row = sqlx::query(
            "SELECT block_body.body FROM block_body \
             JOIN block_record ON block_record.header_hash = block_body.header_hash \
             WHERE block_record.height = $1 AND block_record.in_main_chain = 1",
        )
        .bind(i64::from(h))
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => {
                let body: Vec<u8> = r.try_get("body")?;
                let raw = zstd::decode_all(&body[..])?;
                let block = FullBlock::from_bytes(&mut Cursor::new(&raw[..]), VERSION)?;
                Ok(block.transactions_generator)
            }
            None => Ok(None),
        }
    }

    async fn add_block_records(&self, records: &[BlockRecord]) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        upsert_records(&mut tx, records).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        upsert_records(batch.pg_conn()?, records).await
    }

    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        let tx = self.pool.begin().await?;
        Ok(BatchHandle {
            inner: BatchInner::Postgres(tx),
            _timing: None,
        })
    }

    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        let BatchInner::Postgres(tx) = &mut batch.inner else {
            return Err(wrong_backend());
        };
        for block in blocks {
            let hh = block.header_hash()?;
            let body = zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?;
            sqlx::query(
                "INSERT INTO block_body (header_hash, body) VALUES ($1, $2) \
                 ON CONFLICT(header_hash) DO UPDATE SET body = excluded.body",
            )
            .bind(hh)
            .bind(body)
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    async fn commit(&self, batch: BatchHandle) -> Result<(), StoreError> {
        let BatchInner::Postgres(tx) = batch.inner else {
            return Err(wrong_backend());
        };
        tx.commit().await?;
        Ok(())
    }

    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        let rows = sqlx::query(
            "SELECT r.height AS height FROM block_record r \
             LEFT JOIN block_body b ON r.header_hash = b.header_hash \
             WHERE b.header_hash IS NULL ORDER BY r.height LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<i64, _>("height")? as u32))
            .collect()
    }

    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let links = set_peak_on(&mut tx, new_peak).await?;
        tx.commit().await?;
        Ok(links)
    }

    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        set_peak_on(batch.pg_conn()?, new_peak).await
    }

    async fn extend_peak_in(
        &self,
        batch: &mut BatchHandle,
        extension: &[Bytes32],
        new_height: u32,
    ) -> Result<u64, StoreError> {
        extend_peak_on(batch.pg_conn()?, extension, new_height).await
    }

    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        let row = sqlx::query("SELECT status FROM block_record WHERE header_hash = $1")
            .bind(*hh)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(r) => Ok(BlockStatus::from_u8(r.try_get::<i64, _>("status")? as u8)),
            None => Ok(BlockStatus::Unvalidated),
        }
    }

    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        let mut conn = self.pool.acquire().await?;
        set_status_on(&mut conn, hh, s).await
    }

    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        set_status_on(batch.pg_conn()?, hh, s).await
    }

    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        Ok(Savepoint {
            peak: self.get_peak().await?,
        })
    }

    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let touched = match sp.peak {
            Some((_, height)) => sqlx::query(
                "UPDATE block_record SET in_main_chain = 0 WHERE in_main_chain = 1 AND height > $1",
            )
            .bind(i64::from(height))
            .execute(&mut *tx)
            .await?
            .rows_affected(),
            None => {
                sqlx::query("UPDATE block_record SET in_main_chain = 0 WHERE in_main_chain = 1")
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
            }
        };
        match sp.peak {
            Some((hh, height)) => {
                sqlx::query(
                    "INSERT INTO current_peak (id, header_hash, height) VALUES (0, $1, $2) \
                     ON CONFLICT(id) DO UPDATE SET header_hash = excluded.header_hash, \
                     height = excluded.height",
                )
                .bind(hh)
                .bind(i64::from(height))
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM current_peak WHERE id = 0")
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(touched)
    }

    async fn get_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        // No decode here: the store hands back the opaque SubEpochSegments bytes.
        let row = sqlx::query(
            "SELECT challenge_segments FROM sub_epoch_segments_v3 WHERE ses_block_hash = $1",
        )
        .bind(*ses_hash)
        .fetch_optional(&self.pool)
        .await?;
        match row {
            Some(r) => Ok(Some(r.try_get::<Vec<u8>, _>("challenge_segments")?)),
            None => Ok(None),
        }
    }

    async fn persist_sub_epoch_segments(
        &self,
        ses_hash: &Bytes32,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        // Fixed-arity single-row upsert — the default persistent prepared statement
        // is correct here; do NOT copy the `.persistent(false)` pattern from coin.rs, which
        // exists only because variable-arity multi-row SQL churned sqlx's prepare cache.
        sqlx::query(
            "INSERT INTO sub_epoch_segments_v3 (ses_block_hash, challenge_segments) \
             VALUES ($1, $2) \
             ON CONFLICT(ses_block_hash) DO UPDATE SET challenge_segments = excluded.challenge_segments",
        )
        .bind(*ses_hash)
        .bind(bytes.to_vec())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn build_indexes(&self) -> Result<(), StoreError> {
        // Deferred index build at the sync->tip transition. Statements run one at a time on
        // autocommit — required by CREATE INDEX CONCURRENTLY, which cannot run inside a
        // transaction and never takes a write-blocking lock, so a node that keeps confirming
        // while the build runs is never stalled. The reorg indexes (0006) back rollback_to's
        // range predicates on every profile; the service tier (0003) only exists on a
        // coin-index build.
        // Comment lines are stripped BEFORE the ';' split — a ';' inside a comment must not cut
        // a statement in half.
        //
        // First, repair the stale-btree defect (0009): a leg whose confirmed_index predates the
        // BRIN decision carries a multi-GB btree under the name 0006's IF NOT EXISTS would then
        // silently keep forever.
        convert_confirmed_index_to_brin(&self.pool).await?;
        let mut sql = crate::strip_sql_comments(include_str!(
            "../../migrations/postgres/0006_reorg_indexes.sql"
        ));
        #[cfg(feature = "coin-index")]
        sql.push_str(&crate::strip_sql_comments(include_str!(
            "../../migrations/postgres/0003_service_indexes.sql"
        )));
        // The coin_hint secondary phases with the service tier (0-scan during sync, wallet-only
        // at tip), so it is created HERE, not by 0004 at open — an at-open create would silently
        // rebuild it on a restart mid-catch-up after a shed.
        #[cfg(feature = "hint")]
        sql.push_str(
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS coin_hint_coin_name ON coin_hint (coin_name);",
        );
        // Fresh planner statistics over the fully-synced tables close the build out.
        sql.push_str("ANALYZE coin_record; ANALYZE block_record;");
        for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        Ok(())
    }

    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        // The falling-edge counterpart of `build_indexes` for a deep re-catch-up: drop the
        // service tier AND the spent_index reorg btree. Shedding spent_index (and the partial
        // unspent_by_ph) is what re-enables HOT spend-updates: an UPDATE touching an indexed
        // column can never be HOT, so index-lean re-catch-up saves both the per-coin index
        // maintenance and the vacuum debt.
        // confirmed_index is KEPT: as a BRIN it is near-free to maintain and still serves the
        // rollback DELETE range cheaply. Deep catch-up applies settled history (reorgs are a
        // tip phenomenon); if a reorg is nonetheless requested while shed,
        // `ensure_reorg_indexes` rebuilds the reorg tier on demand before the rollback runs.
        //
        // Plain DROP, not CONCURRENTLY: an interrupted DROP CONCURRENTLY leaves the index
        // INVALID-but-present, which the later `CREATE INDEX CONCURRENTLY IF NOT EXISTS`
        // rebuild then skips forever (the same trap `ensure_reorg_indexes` documents for
        // CREATE) — whereas a killed plain DROP rolls back cleanly. Each drop is a catalog
        // update + file unlink: the ACCESS EXCLUSIVE lock is momentary. One autocommit
        // statement per index; a shed interrupted between statements leaves a subset absent,
        // which the `IF NOT EXISTS` rebuild handles.
        for stmt in [
            "DROP INDEX IF EXISTS coin_record_puzzle_hash",
            "DROP INDEX IF EXISTS coin_record_coin_parent",
            "DROP INDEX IF EXISTS coin_record_unspent_by_ph",
            "DROP INDEX IF EXISTS coin_record_spent_index",
            #[cfg(feature = "hint")]
            "DROP INDEX IF EXISTS coin_hint_coin_name",
        ] {
            sqlx::query(stmt).execute(&self.pool).await?;
        }
        // Repair the stale-btree confirmed_index here too (0009): the deep-behind leg this shed
        // exists for may not see a rising-edge build for months, yet pays the btree's per-insert
        // maintenance on every one of the millions of catch-up coins. Runs LAST — the drops
        // above are momentary catalog ops, while the BRIN replacement is an online CONCURRENTLY
        // heap scan.
        convert_confirmed_index_to_brin(&self.pool).await?;
        Ok(())
    }
}

/// The 0009 btree->BRIN swap for `coin_record_confirmed_index` — see the migration file for the
/// full rationale and crash-safety argument. Guarded: converts only when the live index exists
/// and is NOT a valid BRIN (a stale btree from the old 0001 schema, or an INVALID leftover from
/// a crashed CONCURRENTLY build); a BRIN-native leg pays one catalog probe and no heap scan.
/// Absent index: nothing to convert — the caller's `CREATE ... IF NOT EXISTS` builds the BRIN
/// fresh (any stale temp is still cleared so it cannot shadow a later conversion).
async fn convert_confirmed_index_to_brin(pool: &sqlx::PgPool) -> Result<(), StoreError> {
    use sqlx::Row;
    let live = sqlx::query(
        "SELECT a.amname, i.indisvalid FROM pg_class c \
         JOIN pg_am a ON a.oid = c.relam \
         JOIN pg_index i ON i.indexrelid = c.oid \
         WHERE c.oid = to_regclass('coin_record_confirmed_index')",
    )
    .fetch_optional(pool)
    .await?;
    let needs_swap = match &live {
        Some(row) => row.get::<String, _>(0) != "brin" || !row.get::<bool, _>(1),
        None => false,
    };
    if !needs_swap {
        // Clear any INVALID temp a crashed earlier conversion left, then done.
        sqlx::query("DROP INDEX CONCURRENTLY IF EXISTS coin_record_confirmed_index_brin")
            .execute(pool)
            .await?;
        return Ok(());
    }
    // 0009's online statements, one at a time on autocommit (CONCURRENTLY cannot run inside a
    // transaction): clear a stale temp, then build the replacement BRIN without blocking writes.
    let sql = crate::strip_sql_comments(include_str!(
        "../../migrations/postgres/0009_confirmed_index_brin.sql"
    ));
    for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(pool).await?;
    }
    // The transactional swap: drop the stale index and take over its name as one atomic catalog
    // change (momentary ACCESS EXCLUSIVE — no heap work). A crash before COMMIT rolls back to
    // the old btree plus a finished temp the next conversion pass clears and rebuilds.
    let mut tx = pool.begin().await?;
    sqlx::query("DROP INDEX coin_record_confirmed_index")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "ALTER INDEX coin_record_confirmed_index_brin RENAME TO coin_record_confirmed_index",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}
