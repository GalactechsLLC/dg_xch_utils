mod block;
mod checkpoint;
mod coin;

use crate::error::StoreError;
use crate::telemetry::StoreTelemetry;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{ConnectOptions, Row, SqliteConnection, SqlitePool};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};

/// A sqlx-SQLite backend for the coin + block stores. One single writer connection (sqlite is
/// single-writer) behind an async mutex; a separate WAL read pool so point reads stay lock-free while a
/// write batch is open; and a dedicated checkpointer connection that drains the WAL off the writer.
pub struct SqliteStore {
    read: SqlitePool,
    writer: Arc<Mutex<SqliteConnection>>,
    near_tip: Arc<AtomicBool>,
    checkpointer: tokio::task::JoinHandle<()>,
    // Commit latency by phase + checkpoint activity, rendered by the node's /metrics responder.
    pub(crate) telemetry: Arc<StoreTelemetry>,
    // `<db>-wal`, for the wal_bytes() file-size gauge (the SQLite WAL always lives at this suffix).
    wal_path: PathBuf,
    bulk_cache_kib: Arc<AtomicU64>,
    checkpoint_notify: Arc<Notify>,
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        self.checkpointer.abort();
    }
}

/// Estimated new WAL bytes between background drains, independent of allocated file size.
const WAL_DRAIN_TRIGGER_BYTES: u64 = 128 * 1024 * 1024;

/// WRITER-connection page-cache profile by sync phase (`PRAGMA cache_size`, negative = KiB).
///
/// Bulk catch-up runs 256 MiB: a catch-up commit spans a whole multi-block window, and a cache
/// too small to hold it spills the dirty pages to the WAL before the COMMIT, which is itself a
/// source of WAL growth. Near tip drops back to 64 MiB — per-block commits fit easily.
///
/// The profile is writer-only: the read pool and the checkpointer keep the 64 MiB connect
/// default, so the bulk cache costs one connection, not pool-size multiples.
const WRITER_CACHE_BULK_KIB: i64 = 262_144;
const WRITER_CACHE_NEAR_TIP_KIB: i64 = 65_536;

impl SqliteStore {
    /// Open (creating if missing) a WAL-mode store at `path` and apply the migrations for the enabled
    /// feature tier.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the database cannot be opened or migrated.
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_with_wal_drain_trigger(path, WAL_DRAIN_TRIGGER_BYTES).await
    }

    /// [`Self::open`] with an explicit new-write budget for background checkpoints.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the database cannot be opened or migrated.
    pub async fn open_with_wal_drain_trigger(
        path: &Path,
        wal_drain_trigger_bytes: u64,
    ) -> Result<Self, StoreError> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true)
            .pragma("mmap_size", "268435456")
            .pragma("temp_store", "MEMORY")
            // Page cache, the CONNECT default (read pool + checkpointer). The SQLite default
            // (-2000 = 2 MiB) forces near-constant cache-spill to the WAL when a confirm writes
            // thousands of random `coin_name` (hash-keyed, WITHOUT ROWID) rows into the multi-GB
            // coin_record b-tree; 64 MiB holds a single block's working set. The WRITER is
            // re-profiled by phase on top of this — bulk 256 MiB / near-tip 64 MiB (see
            // WRITER_CACHE_BULK_KIB) — because catch-up batch commits span many blocks and spill
            // at 64 MiB.
            .pragma("cache_size", "-65536")
            // Keep the writer-COMMIT autocheckpoint out of normal operation: an autocheckpoint
            // firing inside a confirm COMMIT copies the whole accumulated WAL into the DB file
            // and fsyncs it on the hot path. The dedicated checkpointer below does that
            // copy+fsync off the writer. This threshold (262 144 pages ≈ 1 GiB) is only a
            // disk-fill failsafe for when that background task is gone.
            .pragma("wal_autocheckpoint", "262144");
        let mut writer = opts.clone().connect().await?;
        // The store opens in the bulk (catch-up) phase; `set_near_tip` re-profiles on each flip.
        sqlx::query(&format!("PRAGMA cache_size = -{WRITER_CACHE_BULK_KIB}"))
            .execute(&mut writer)
            .await?;
        migrate(&mut writer).await?;
        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .idle_timeout(Duration::from_secs(60))
            .max_lifetime(Duration::from_secs(600))
            .connect_with(opts.clone().read_only(true))
            .await?;
        let near_tip = Arc::new(AtomicBool::new(false));
        let telemetry = StoreTelemetry::new();
        telemetry
            .writer_cache_kib
            .store(WRITER_CACHE_BULK_KIB as u64, Ordering::Relaxed);
        // SQLite's WAL file always lives at `<db>-wal` (same directory, suffix appended).
        let mut wal_os = path.as_os_str().to_os_string();
        wal_os.push("-wal");
        let wal_path = PathBuf::from(wal_os);
        let page_size: i64 = sqlx::query_scalar("PRAGMA page_size")
            .fetch_one(&mut writer)
            .await?;
        let writer = Arc::new(Mutex::new(writer));
        let checkpoint_notify = Arc::new(Notify::new());
        let checkpointer = checkpoint::spawn_checkpointer(
            opts.busy_timeout(Duration::ZERO).connect().await?,
            near_tip.clone(),
            telemetry.clone(),
            read.clone(),
            writer.clone(),
            checkpoint_notify.clone(),
            wal_drain_trigger_bytes,
            page_size as u64,
        );
        Ok(Self {
            read,
            writer,
            near_tip,
            checkpointer,
            telemetry,
            wal_path,
            bulk_cache_kib: Arc::new(AtomicU64::new(WRITER_CACHE_BULK_KIB as u64)),
            checkpoint_notify,
        })
    }

    /// Current size in bytes of the `-wal` file (0 when it does not exist yet). A metadata stat —
    /// cheap enough for every scrape.
    #[must_use]
    pub fn wal_file_bytes(&self) -> u64 {
        std::fs::metadata(&self.wal_path).map_or(0, |m| m.len())
    }

    /// The WRITER connection's current `PRAGMA cache_size` (negative = KiB, SQLite's
    /// convention) — the phase-profile probe: bulk catch-up runs the large cache, near-tip the
    /// small one. Diagnostic/test seam; takes the writer lock briefly.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the pragma query fails.
    pub async fn writer_cache_size(&self) -> Result<i64, StoreError> {
        let mut guard = self.writer.lock().await;
        let row = sqlx::query("PRAGMA cache_size")
            .fetch_one(&mut *guard)
            .await?;
        Ok(row.try_get(0)?)
    }

    /// Re-apply the phase-appropriate writer cache profile (see [`WRITER_CACHE_BULK_KIB`]).
    /// Called from `set_near_tip` (a sync trait method), so the pragma runs on a spawned task
    /// that takes the writer lock; it reads the CURRENT phase at execution time, so racing
    /// flips converge on the latest phase. `PRAGMA cache_size` takes effect immediately on the
    /// connection; shrinking releases the pages lazily.
    pub async fn set_bulk_cache_kib(&self, kib: u64) -> Result<(), StoreError> {
        if kib == 0 || kib > i32::MAX as u64 {
            return Err(StoreError::Batch("writer cache KiB is out of range".into()));
        }
        let mut guard = self.writer.lock().await;
        self.bulk_cache_kib.store(kib, Ordering::Relaxed);
        if !self.near_tip.load(Ordering::Relaxed) {
            sqlx::query(&format!("PRAGMA cache_size = -{kib}"))
                .execute(&mut *guard)
                .await?;
            self.telemetry
                .writer_cache_kib
                .store(kib, Ordering::Relaxed);
        }
        Ok(())
    }

    pub(crate) fn apply_writer_cache_profile(&self) {
        let writer = self.writer.clone();
        let near_tip = self.near_tip.clone();
        let bulk_cache_kib = self.bulk_cache_kib.clone();
        let telemetry = self.telemetry.clone();
        tokio::spawn(async move {
            let mut guard = writer.lock().await;
            let kib = if near_tip.load(Ordering::Relaxed) {
                WRITER_CACHE_NEAR_TIP_KIB as u64
            } else {
                bulk_cache_kib.load(Ordering::Relaxed)
            };
            if let Err(e) = sqlx::query(&format!("PRAGMA cache_size = -{kib}"))
                .execute(&mut *guard)
                .await
            {
                log::warn!("writer cache re-profile to -{kib} KiB failed: {e}");
            } else {
                telemetry.writer_cache_kib.store(kib, Ordering::Relaxed);
            }
        });
    }
}

async fn migrate(conn: &mut SqliteConnection) -> Result<(), StoreError> {
    sqlx::raw_sql(include_str!("../../migrations/sqlite/0001_coin_record.sql"))
        .execute(&mut *conn)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/sqlite/0002_block.sql"))
        .execute(&mut *conn)
        .await?;
    // 0003 (service indexes) and 0006 (reorg indexes) are deferred to `build_indexes` at the
    // sync->tip transition: secondary coin_record indexes are pure write-amplification during
    // sync (see the postgres migrate note).
    #[cfg(feature = "hint")]
    sqlx::raw_sql(include_str!("../../migrations/sqlite/0004_hint.sql"))
        .execute(&mut *conn)
        .await?;
    sqlx::raw_sql(include_str!(
        "../../migrations/sqlite/0005_sub_epoch_segments.sql"
    ))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub(crate) fn amount_be(amount: u64) -> Vec<u8> {
    amount.to_be_bytes().to_vec()
}

pub(crate) fn amount_from_be(bytes: &[u8]) -> Result<u64, StoreError> {
    let arr: [u8; 8] = bytes.try_into().map_err(|_| {
        StoreError::Corrupt(format!("amount blob is {} bytes, want 8", bytes.len()))
    })?;
    Ok(u64::from_be_bytes(arr))
}

fn row_to_coin_record(row: &sqlx::sqlite::SqliteRow) -> Result<CoinRecord, StoreError> {
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
