use crate::error::StoreError;
use crate::sqlite::SqliteStore;
use crate::traits::BlockStore;
use crate::types::{BatchHandle, BlockStatus, Savepoint};
use async_trait::async_trait;
use dg_xch_core::blockchain::block_record::BlockRecord;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use sqlx::{Connection, Row};
use std::io::Cursor;
use std::sync::atomic::Ordering;

const VERSION: ChiaProtocolVersion = ChiaProtocolVersion::Chia0_0_37;

const UPSERT_RECORD: &str = "INSERT INTO block_record \
    (header_hash, prev_hash, height, weight, total_iters, is_transaction_block, sub_epoch_summary, record) \
    VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
    ON CONFLICT(header_hash) DO UPDATE SET prev_hash = excluded.prev_hash, height = excluded.height, \
    weight = excluded.weight, total_iters = excluded.total_iters, \
    is_transaction_block = excluded.is_transaction_block, sub_epoch_summary = excluded.sub_epoch_summary, \
    record = excluded.record";

// Legacy-tolerant: older blobs decode via the fallback walk; see record_compat.rs.
use crate::record_compat::decode_record;

// Shared write bodies, parameterized over the connection so the same statements run either in a
// self-contained transaction (the standalone trait methods) or joined onto an open batch (the `_in`
// variants — the one-fsync-per-block apply path).
async fn upsert_records(
    conn: &mut sqlx::SqliteConnection,
    records: &[BlockRecord],
) -> Result<(), StoreError> {
    if records.len() == 1 {
        let r = &records[0];
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
            .execute(conn)
            .await?;
        return Ok(());
    }
    // Eight binds per row; 100 rows stays below SQLite's conservative 999-variable limit.
    for chunk in records.chunks(100) {
        let mut encoded = Vec::with_capacity(chunk.len());
        for r in chunk {
            encoded.push((
                r.header_hash,
                r.prev_hash,
                i64::from(r.height),
                r.weight.to_be_bytes().to_vec(),
                r.total_iters.to_be_bytes().to_vec(),
                i64::from(r.is_transaction_block()),
                r.sub_epoch_summary_included
                    .as_ref()
                    .map(|s| s.to_bytes(VERSION))
                    .transpose()?,
                r.to_bytes(VERSION)?,
            ));
        }
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO block_record \
             (header_hash, prev_hash, height, weight, total_iters, is_transaction_block, \
             sub_epoch_summary, record) ",
        );
        qb.push_values(encoded, |mut b, row| {
            b.push_bind(row.0)
                .push_bind(row.1)
                .push_bind(row.2)
                .push_bind(row.3)
                .push_bind(row.4)
                .push_bind(row.5)
                .push_bind(row.6)
                .push_bind(row.7);
        });
        qb.push(
            " ON CONFLICT(header_hash) DO UPDATE SET prev_hash = excluded.prev_hash, \
             height = excluded.height, weight = excluded.weight, total_iters = excluded.total_iters, \
             is_transaction_block = excluded.is_transaction_block, \
             sub_epoch_summary = excluded.sub_epoch_summary, record = excluded.record",
        );
        qb.build().persistent(false).execute(&mut *conn).await?;
    }
    Ok(())
}

async fn set_status_on(
    conn: &mut sqlx::SqliteConnection,
    hh: &Bytes32,
    s: BlockStatus,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE block_record SET status = ? WHERE header_hash = ?")
        .bind(i64::from(s.as_u8()))
        .bind(*hh)
        .execute(conn)
        .await?;
    Ok(())
}

async fn set_peak_on(
    conn: &mut sqlx::SqliteConnection,
    new_peak: &Bytes32,
) -> Result<u64, StoreError> {
    let Some(row) = sqlx::query("SELECT height FROM block_record WHERE header_hash = ?")
        .bind(*new_peak)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Err(StoreError::Corrupt(
            "set_peak: unknown header hash".to_string(),
        ));
    };
    let new_height: i64 = row.try_get("height")?;
    // Fork point: the deepest ancestor of new_peak already on the main chain. Walk the new branch's
    // ancestry (all in_main_chain = 0 until the fork) to find it; -1 = no shared ancestor (full replace).
    let mut fork_height = -1i64;
    let mut cursor = *new_peak;
    loop {
        let Some(r) = sqlx::query(
            "SELECT prev_hash, height, in_main_chain FROM block_record WHERE header_hash = ?",
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
    // Retire the whole old main branch above the fork — not just above the new peak — so a same-height or
    // shorter-heavier reorg cannot leave an abandoned sibling flagged (T012a).
    sqlx::query("UPDATE block_record SET in_main_chain = 0 WHERE in_main_chain = 1 AND height > ?")
        .bind(fork_height)
        .execute(&mut *conn)
        .await?;
    // Link the new ancestry onto the main chain, back to (but not including) the fork ancestor.
    let mut links = 0u64;
    let mut cursor = *new_peak;
    loop {
        let Some(r) =
            sqlx::query("SELECT prev_hash, in_main_chain FROM block_record WHERE header_hash = ?")
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
        sqlx::query("UPDATE block_record SET in_main_chain = 1 WHERE header_hash = ?")
            .bind(cursor)
            .execute(&mut *conn)
            .await?;
        links += 1;
        cursor = prev;
    }
    sqlx::query(
        "INSERT INTO current_peak (id, header_hash, height) VALUES (0, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET header_hash = excluded.header_hash, height = excluded.height",
    )
    .bind(*new_peak)
    .bind(new_height)
    .execute(&mut *conn)
    .await?;
    Ok(links)
}

// Fast path for a chain segment whose parent/weight continuity the engine already proved. The
// general set_peak_on walk issues a SELECT + UPDATE for every new height to rediscover the fork;
// a plain extension has no fork, so update the exact validated hashes in one bounded statement.
async fn extend_peak_on(
    conn: &mut sqlx::SqliteConnection,
    extension: &[Bytes32],
    new_height: u32,
) -> Result<u64, StoreError> {
    let Some(new_peak) = extension.last() else {
        return Ok(0);
    };
    let mut links = 0u64;
    // Stay below SQLite's conservative 999-variable limit even on older deployments.
    for chunk in extension.chunks(900) {
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
        "INSERT INTO current_peak (id, header_hash, height) VALUES (0, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET header_hash = excluded.header_hash, height = excluded.height",
    )
    .bind(*new_peak)
    .bind(i64::from(new_height))
    .execute(conn)
    .await?;
    Ok(links)
}

#[async_trait]
impl BlockStore for SqliteStore {
    async fn prepare_archive(
        &self,
        records: Vec<(BlockRecord, BlockStatus)>,
        blocks: Vec<FullBlock>,
    ) -> Result<crate::types::PreparedArchive, StoreError> {
        let telemetry = self.telemetry.clone();
        tokio::task::spawn_blocking(move || {
            let _timer = telemetry.archive_prepare.start();
            let encoded = dg_xch_core::compute::map(
                dg_xch_core::compute::Phase::Archive,
                &records,
                |(record, status)| {
                    Ok::<_, StoreError>(crate::types::EncodedRecord {
                        hash: record.header_hash,
                        parent: record.prev_hash,
                        height: i64::from(record.height),
                        weight: record.weight.to_be_bytes().to_vec(),
                        iterations: record.total_iters.to_be_bytes().to_vec(),
                        transaction: i64::from(record.is_transaction_block()),
                        summary: record
                            .sub_epoch_summary_included
                            .as_ref()
                            .map(|summary| summary.to_bytes(VERSION))
                            .transpose()?,
                        record: record.to_bytes(VERSION)?,
                        status: i64::from(status.as_u8()),
                    })
                },
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
            let bodies =
                dg_xch_core::compute::map(dg_xch_core::compute::Phase::Archive, &blocks, |block| {
                    Ok::<_, StoreError>((
                        block.header_hash()?,
                        zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?,
                    ))
                })
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?;
            let bytes: usize = bodies.iter().map(|(_, body)| body.len()).sum::<usize>()
                + encoded
                    .iter()
                    .map(|record| record.record.len())
                    .sum::<usize>();
            telemetry
                .prepared_bytes
                .fetch_add(bytes as u64, Ordering::Relaxed);
            Ok(crate::types::PreparedArchive::Sqlite {
                records: encoded,
                bodies,
            })
        })
        .await
        .map_err(|error| StoreError::Batch(format!("archive preparation: {error}")))?
    }

    async fn persist_prepared_archive_in(
        &self,
        batch: &mut BatchHandle,
        prepared: crate::types::PreparedArchive,
    ) -> Result<(), StoreError> {
        let crate::types::PreparedArchive::Sqlite { records, bodies } = prepared else {
            return Err(StoreError::Batch(
                "archive prepared by another backend".into(),
            ));
        };
        let _timer = self.telemetry.archive_write.start();
        let conn = batch.sqlite_conn()?;
        for chunk in records.chunks(100) {
            let mut query = sqlx::QueryBuilder::new(
                "INSERT INTO block_record (header_hash, prev_hash, height, weight, total_iters, \
                 is_transaction_block, sub_epoch_summary, record, status) ",
            );
            query.push_values(chunk, |mut bindings, record| {
                bindings
                    .push_bind(record.hash)
                    .push_bind(record.parent)
                    .push_bind(record.height)
                    .push_bind(&record.weight)
                    .push_bind(&record.iterations)
                    .push_bind(record.transaction)
                    .push_bind(&record.summary)
                    .push_bind(&record.record)
                    .push_bind(record.status);
            });
            query.push(" ON CONFLICT(header_hash) DO UPDATE SET prev_hash=excluded.prev_hash, \
                height=excluded.height, weight=excluded.weight, total_iters=excluded.total_iters, \
                is_transaction_block=excluded.is_transaction_block, \
                sub_epoch_summary=excluded.sub_epoch_summary, record=excluded.record, status=excluded.status");
            query
                .build()
                .persistent(chunk.len() == 100)
                .execute(&mut *conn)
                .await?;
            self.telemetry.archive_write.statement(chunk.len());
        }
        let mut start = 0;
        while start < bodies.len() {
            let mut end = start;
            let mut bytes = 0usize;
            while end < bodies.len() && end - start < 16 {
                let next = bodies[end].1.len();
                if end > start && bytes.saturating_add(next) > 16 * 1024 * 1024 {
                    break;
                }
                bytes = bytes.saturating_add(next);
                end += 1;
            }
            let mut query =
                sqlx::QueryBuilder::new("INSERT OR REPLACE INTO block_body (header_hash, body) ");
            query.push_values(&bodies[start..end], |mut bindings, (hash, body)| {
                bindings.push_bind(*hash).push_bind(body);
            });
            query
                .build()
                .persistent(end - start == 16)
                .execute(&mut *conn)
                .await?;
            self.telemetry.archive_write.statement(end - start);
            start = end;
        }
        Ok(())
    }

    async fn get_block_record(&self, hh: &Bytes32) -> Result<Option<BlockRecord>, StoreError> {
        self.telemetry.record_reads.fetch_add(1, Ordering::Relaxed);
        let row = sqlx::query("SELECT record FROM block_record WHERE header_hash = ?")
            .bind(*hh)
            .fetch_optional(&self.read)
            .await?;
        match row {
            Some(r) => Ok(Some(decode_record(&r.try_get::<Vec<u8>, _>("record")?)?)),
            None => Ok(None),
        }
    }

    async fn get_block_records_by_hash(
        &self,
        hashes: &[Bytes32],
    ) -> Result<Vec<BlockRecord>, StoreError> {
        if hashes.is_empty() {
            return Ok(Vec::new());
        }
        self.telemetry
            .record_reads
            .fetch_add(hashes.len() as u64, Ordering::Relaxed);
        // Point-get each hash over the primary key on ONE acquired read connection — the
        // `get_coin_records` shape (see that method's planner note: a dynamic `IN (...)` list
        // collapses to a full table scan past SQLite's search-vs-scan crossover; a `= ?` PK
        // lookup can never regress). One pool acquire and one read snapshot for the whole batch.
        let mut conn = self.read.acquire().await?;
        let mut out = Vec::with_capacity(hashes.len());
        for hh in hashes {
            if let Some(row) = sqlx::query("SELECT record FROM block_record WHERE header_hash = ?")
                .bind(*hh)
                .fetch_optional(&mut *conn)
                .await?
            {
                out.push(decode_record(&row.try_get::<Vec<u8>, _>("record")?)?);
            }
        }
        Ok(out)
    }

    async fn get_block_record_by_height(&self, h: u32) -> Result<Option<BlockRecord>, StoreError> {
        self.telemetry.record_reads.fetch_add(1, Ordering::Relaxed);
        let row =
            sqlx::query("SELECT record FROM block_record WHERE height = ? AND in_main_chain = 1")
                .bind(i64::from(h))
                .fetch_optional(&self.read)
                .await?;
        match row {
            Some(r) => Ok(Some(decode_record(&r.try_get::<Vec<u8>, _>("record")?)?)),
            None => Ok(None),
        }
    }

    async fn get_peak(&self) -> Result<Option<(Bytes32, u32)>, StoreError> {
        let row = sqlx::query("SELECT header_hash, height FROM current_peak WHERE id = 0")
            .fetch_optional(&self.read)
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
        // SQLite's MIN/MAX optimization answers this from the first entry of the partial
        // `block_record_height_main` index — no scan, cheap enough for every /metrics scrape.
        let row = sqlx::query("SELECT MIN(height) AS h FROM block_record WHERE in_main_chain = 1")
            .fetch_one(&self.read)
            .await?;
        let h: Option<i64> = row.try_get("h")?;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        Ok(h.map(|v| v as u32))
    }

    async fn get_block(&self, hh: &Bytes32) -> Result<Option<FullBlock>, StoreError> {
        let row = sqlx::query("SELECT body FROM block_body WHERE header_hash = ?")
            .bind(*hh)
            .fetch_optional(&self.read)
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
        // Single confirmed-main-chain join: the referenced generator lives in the block occupying
        // `h` on the confirmed chain. No cheap generator-only parser exists, so the cold body is
        // decompressed and decoded to the FullBlock, then its generator is returned.
        let row = sqlx::query(
            "SELECT block_body.body FROM block_body \
             JOIN block_record ON block_record.header_hash = block_body.header_hash \
             WHERE block_record.height = ? AND block_record.in_main_chain = 1",
        )
        .bind(i64::from(h))
        .fetch_optional(&self.read)
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
        let mut guard = self.writer.lock().await;
        let mut tx = guard.begin().await?;
        upsert_records(&mut tx, records).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn add_block_records_in(
        &self,
        batch: &mut BatchHandle,
        records: &[BlockRecord],
    ) -> Result<(), StoreError> {
        upsert_records(batch.sqlite_conn()?, records).await
    }

    async fn begin(&self) -> Result<BatchHandle, StoreError> {
        let wait = self.telemetry.writer_wait.start();
        let mut guard = self.writer.clone().lock_owned().await;
        drop(wait);
        let timing = Some(self.telemetry.writer_hold.start());
        // A BatchHandle dropped without a commit (e.g. a mid-window per-block confirm error) leaves
        // its BEGIN open on the single writer connection; without this a later begin would fail with
        // "cannot start a transaction within a transaction" and wedge the writer forever. Best-effort
        // clear any such dangling transaction first (the ROLLBACK is a harmless no-op error when the
        // connection is already clean), so begin can never wedge on a prior failure.
        let _ = sqlx::query("ROLLBACK").execute(&mut *guard).await;
        sqlx::query("BEGIN").execute(&mut *guard).await?;
        Ok(BatchHandle {
            inner: crate::types::BatchInner::Sqlite(guard),
            _timing: timing,
        })
    }

    async fn append_many(
        &self,
        batch: &mut BatchHandle,
        blocks: &[FullBlock],
    ) -> Result<(), StoreError> {
        // Irrefutable without the `postgres` feature (BatchInner then has a single variant).
        #[allow(irrefutable_let_patterns)]
        let crate::types::BatchInner::Sqlite(conn) = &mut batch.inner else {
            return Err(StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        if blocks.len() == 1 {
            let block = &blocks[0];
            sqlx::query("INSERT OR REPLACE INTO block_body (header_hash, body) VALUES (?, ?)")
                .bind(block.header_hash()?)
                .bind(zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?)
                .execute(&mut **conn)
                .await?;
            return Ok(());
        }
        // Keep statements body-size bounded: later-chain blocks can approach 1 MiB each.
        for chunk in blocks.chunks(16) {
            let mut encoded = Vec::with_capacity(chunk.len());
            for block in chunk {
                encoded.push((
                    block.header_hash()?,
                    zstd::encode_all(&block.to_bytes(VERSION)?[..], 3)?,
                ));
            }
            let mut qb =
                sqlx::QueryBuilder::new("INSERT OR REPLACE INTO block_body (header_hash, body) ");
            qb.push_values(encoded, |mut b, (hash, body)| {
                b.push_bind(hash).push_bind(body);
            });
            qb.build().persistent(false).execute(&mut **conn).await?;
        }
        Ok(())
    }

    async fn commit(&self, mut batch: BatchHandle) -> Result<(), StoreError> {
        // Irrefutable without the `postgres` feature (BatchInner then has a single variant).
        #[allow(irrefutable_let_patterns)]
        let crate::types::BatchInner::Sqlite(conn) = &mut batch.inner else {
            return Err(StoreError::Corrupt(
                "batch was opened by a different backend".to_string(),
            ));
        };
        // Telemetry hook: time the COMMIT (the fsync-bearing statement) and file it under the
        // CURRENT phase — the near_tip flag at commit time. A batch begun just before a band flip is
        // mislabelled by at most that one commit; the trend queries this feeds are unaffected.
        let started = std::time::Instant::now();
        sqlx::query("COMMIT").execute(&mut **conn).await?;
        let phase_near_tip = self.near_tip.load(Ordering::Relaxed);
        let hist = if phase_near_tip {
            &self.telemetry.commit_near_tip
        } else {
            &self.telemetry.commit_catch_up
        };
        hist.record(started.elapsed().as_secs_f64());
        self.telemetry.last_commit_unix.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            Ordering::Relaxed,
        );
        if let Ok(mut handle) = conn.lock_handle().await {
            let database = handle.as_raw_handle().as_ptr();
            for (kind, counter) in [
                (
                    libsqlite3_sys::SQLITE_DBSTATUS_CACHE_HIT,
                    &self.telemetry.cache_hits,
                ),
                (
                    libsqlite3_sys::SQLITE_DBSTATUS_CACHE_MISS,
                    &self.telemetry.cache_misses,
                ),
                (
                    libsqlite3_sys::SQLITE_DBSTATUS_CACHE_WRITE,
                    &self.telemetry.cache_writes,
                ),
                (
                    libsqlite3_sys::SQLITE_DBSTATUS_CACHE_SPILL,
                    &self.telemetry.cache_spills,
                ),
            ] {
                let mut current = 0;
                let mut highwater = 0;
                if unsafe {
                    libsqlite3_sys::sqlite3_db_status(
                        database,
                        kind,
                        &mut current,
                        &mut highwater,
                        1,
                    )
                } == 0
                {
                    counter.fetch_add(current.max(0) as u64, Ordering::Relaxed);
                }
            }
        }
        Ok(())
    }

    fn near_tip(&self) -> bool {
        self.near_tip.load(Ordering::Relaxed)
    }

    fn set_near_tip(&self, near_tip: bool) {
        let was = self.near_tip.swap(near_tip, Ordering::Relaxed);
        if was != near_tip {
            // Phase flip: swap the writer between the bulk (256 MiB) and near-tip (64 MiB)
            // cache profiles — catch-up batch commits span many blocks and spill the small
            // cache to the WAL (see WRITER_CACHE_BULK_KIB in mod.rs).
            self.apply_writer_cache_profile();
        }
    }

    fn telemetry(&self) -> Option<std::sync::Arc<crate::telemetry::StoreTelemetry>> {
        Some(self.telemetry.clone())
    }

    fn wal_bytes(&self) -> u64 {
        self.wal_file_bytes()
    }

    async fn get_unassociated(&self, limit: usize) -> Result<Vec<u32>, StoreError> {
        let rows = sqlx::query(
            "SELECT r.height AS height FROM block_record r \
             LEFT JOIN block_body b ON r.header_hash = b.header_hash \
             WHERE b.header_hash IS NULL ORDER BY r.height LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.read)
        .await?;
        rows.iter()
            .map(|r| Ok(r.try_get::<i64, _>("height")? as u32))
            .collect()
    }

    async fn set_peak(&self, new_peak: &Bytes32) -> Result<u64, StoreError> {
        let mut guard = self.writer.lock().await;
        let mut tx = guard.begin().await?;
        let links = set_peak_on(&mut tx, new_peak).await?;
        tx.commit().await?;
        Ok(links)
    }

    async fn set_peak_in(
        &self,
        batch: &mut BatchHandle,
        new_peak: &Bytes32,
    ) -> Result<u64, StoreError> {
        let _timer = self.telemetry.peak_update.start();
        set_peak_on(batch.sqlite_conn()?, new_peak).await
    }

    async fn extend_peak_in(
        &self,
        batch: &mut BatchHandle,
        extension: &[Bytes32],
        new_height: u32,
    ) -> Result<u64, StoreError> {
        let _timer = self.telemetry.peak_update.start();
        extend_peak_on(batch.sqlite_conn()?, extension, new_height).await
    }

    async fn get_status(&self, hh: &Bytes32) -> Result<BlockStatus, StoreError> {
        let row = sqlx::query("SELECT status FROM block_record WHERE header_hash = ?")
            .bind(*hh)
            .fetch_optional(&self.read)
            .await?;
        match row {
            Some(r) => Ok(BlockStatus::from_u8(r.try_get::<i64, _>("status")? as u8)),
            None => Ok(BlockStatus::Unvalidated),
        }
    }

    async fn set_status(&self, hh: &Bytes32, s: BlockStatus) -> Result<(), StoreError> {
        let mut guard = self.writer.lock().await;
        set_status_on(&mut guard, hh, s).await
    }

    async fn set_status_in(
        &self,
        batch: &mut BatchHandle,
        hh: &Bytes32,
        s: BlockStatus,
    ) -> Result<(), StoreError> {
        set_status_on(batch.sqlite_conn()?, hh, s).await
    }

    async fn set_status_many_in(
        &self,
        batch: &mut BatchHandle,
        hashes: &[Bytes32],
        status: BlockStatus,
    ) -> Result<(), StoreError> {
        let conn = batch.sqlite_conn()?;
        for chunk in hashes.chunks(900) {
            let mut qb = sqlx::QueryBuilder::new("UPDATE block_record SET status = ");
            qb.push_bind(i64::from(status.as_u8()));
            qb.push(" WHERE header_hash IN (");
            let mut separated = qb.separated(", ");
            for hash in chunk {
                separated.push_bind(*hash);
            }
            qb.push(")");
            qb.build().persistent(false).execute(&mut *conn).await?;
        }
        Ok(())
    }

    async fn savepoint(&self) -> Result<Savepoint, StoreError> {
        Ok(Savepoint {
            peak: self.get_peak().await?,
        })
    }

    async fn rollback(&self, sp: Savepoint) -> Result<u64, StoreError> {
        let mut guard = self.writer.lock().await;
        let mut tx = guard.begin().await?;
        let touched = match sp.peak {
            Some((_, height)) => sqlx::query(
                "UPDATE block_record SET in_main_chain = 0 WHERE in_main_chain = 1 AND height > ?",
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
                    "INSERT INTO current_peak (id, header_hash, height) VALUES (0, ?, ?) \
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
            "SELECT challenge_segments FROM sub_epoch_segments_v3 WHERE ses_block_hash = ?",
        )
        .bind(*ses_hash)
        .fetch_optional(&self.read)
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
        // INSERT OR REPLACE, one row per ses block hash. Fixed-arity statement, so the default
        // persistent prepared statement is correct here; only variable-arity multi-row SQL churns
        // the prepare cache (see postgres/coin.rs).
        let mut guard = self.writer.lock().await;
        sqlx::query(
            "INSERT OR REPLACE INTO sub_epoch_segments_v3 (ses_block_hash, challenge_segments) \
             VALUES (?, ?)",
        )
        .bind(*ses_hash)
        .bind(bytes.to_vec())
        .execute(&mut *guard)
        .await?;
        Ok(())
    }

    async fn build_indexes(&self) -> Result<(), StoreError> {
        // Deferred index build at the sync->tip transition. One statement per writer-lock
        // acquisition so confirms interleave between index builds instead of stalling behind
        // one long guard. The reorg indexes (0006) back rollback_to's range predicates on
        // every profile; the service tier (0003) only exists on a coin-index build.
        // Comment lines are stripped BEFORE the ';' split — a ';' inside a comment must not cut
        // a statement in half.
        let sql = crate::strip_sql_comments(include_str!(
            "../../migrations/sqlite/0006_reorg_indexes.sql"
        ));
        #[cfg(feature = "coin-index")]
        let sql = sql
            + &crate::strip_sql_comments(include_str!(
                "../../migrations/sqlite/0003_service_indexes.sql"
            ));
        // The coin_hint secondary phases with the service tier (0-scan during sync, wallet-only
        // at tip), so it is created HERE, not by 0004 at open — an at-open create would silently
        // rebuild it on a restart mid-catch-up after a shed.
        #[cfg(feature = "hint")]
        let sql = sql + "CREATE INDEX IF NOT EXISTS coin_hint_coin_name ON coin_hint (coin_name);";
        for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let mut guard = self.writer.lock().await;
            sqlx::query(stmt).execute(&mut *guard).await?;
        }
        Ok(())
    }

    async fn shed_service_indexes(&self) -> Result<(), StoreError> {
        // The falling-edge counterpart of `build_indexes` for a deep re-catch-up: return the
        // store to the index-lean bulk-sync posture (coin_record pkey only). Unlike the
        // Postgres twin, `confirmed_index` is shed too — SQLite has no BRIN, so here it is a
        // full btree paying random maintenance per insert, exactly the write-amplification the
        // shed exists to remove; `ensure_reorg_indexes` restores both reorg btrees on demand if
        // a reorg is nonetheless requested while shed. One statement per writer-lock
        // acquisition (the `build_indexes` pattern) so confirms interleave; a shed interrupted
        // part-way leaves a subset absent, which the `IF NOT EXISTS` rebuild handles.
        for stmt in [
            "DROP INDEX IF EXISTS coin_record_puzzle_hash",
            "DROP INDEX IF EXISTS coin_record_coin_parent",
            "DROP INDEX IF EXISTS coin_record_unspent_by_ph",
            "DROP INDEX IF EXISTS coin_record_spent_index",
            "DROP INDEX IF EXISTS coin_record_confirmed_index",
            #[cfg(feature = "hint")]
            "DROP INDEX IF EXISTS coin_hint_coin_name",
        ] {
            let mut guard = self.writer.lock().await;
            sqlx::query(stmt).execute(&mut *guard).await?;
        }
        Ok(())
    }
}
