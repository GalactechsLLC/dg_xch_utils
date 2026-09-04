mod block;
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;

/// A sqlx-SQLite backend for the coin + block stores. One single writer connection (sqlite is
/// single-writer) behind an async mutex; a separate WAL read pool so point reads stay lock-free while a
/// write batch is open; and a dedicated checkpointer connection that drains the WAL off the writer.
pub struct SqliteStore {
    read: SqlitePool,
    writer: Arc<Mutex<SqliteConnection>>,
    // Phase signal read by the checkpointer + the confirm loop: true near the tip (per-block commits +
    // active checkpointer), false during bulk catch-up (big batch commits + quiet checkpointer).
    near_tip: Arc<AtomicBool>,
    // Background WAL checkpointer. Aborted on drop; see `spawn_checkpointer`.
    checkpointer: tokio::task::JoinHandle<()>,
    // Commit latency by phase + checkpoint activity, rendered by the node's /metrics responder.
    pub(crate) telemetry: Arc<StoreTelemetry>,
    // `<db>-wal`, for the wal_bytes() file-size gauge (the SQLite WAL always lives at this suffix).
    wal_path: PathBuf,
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        self.checkpointer.abort();
    }
}

/// Size trigger for the off-writer WAL drain (see `spawn_checkpointer`): when the `-wal` file
/// crosses this many bytes the checkpointer drains NOW, regardless of the bulk-phase cadence,
/// escalating from PASSIVE to TRUNCATE until the file is back under the trigger. Must stay
/// well under the in-writer `wal_autocheckpoint` failsafe (~1 GiB), whose blocking
/// copy-into-DB runs inside a confirm COMMIT.
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

    /// [`Self::open`] with an explicit WAL drain trigger (bytes) — the size at which the
    /// background checkpointer drains immediately instead of waiting for the bulk cadence.
    /// Production uses [`WAL_DRAIN_TRIGGER_BYTES`]; tests shrink it to exercise the drain
    /// without writing 128 MiB.
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
        // Read-pool connections are RECYCLED (idle_timeout + max_lifetime): a pooled WAL reader
        // sitting on an old read mark pins the checkpoint reset point, so PASSIVE can copy
        // frames into the DB but never reset the write pointer and the `-wal` file grows without
        // bound. Bounding every reader's lifetime bounds the pin to at most idle_timeout,
        // letting the reset (and the size-triggered TRUNCATE) succeed.
        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .idle_timeout(Duration::from_secs(60))
            .max_lifetime(Duration::from_secs(600))
            .connect_with(opts.clone().read_only(true))
            .await?;
        let near_tip = Arc::new(AtomicBool::new(false));
        let telemetry = StoreTelemetry::new();
        // SQLite's WAL file always lives at `<db>-wal` (same directory, suffix appended).
        let mut wal_os = path.as_os_str().to_os_string();
        wal_os.push("-wal");
        let wal_path = PathBuf::from(wal_os);
        let checkpointer = spawn_checkpointer(
            opts.connect().await?,
            near_tip.clone(),
            telemetry.clone(),
            read.clone(),
            wal_path.clone(),
            wal_drain_trigger_bytes,
        );
        Ok(Self {
            read,
            writer: Arc::new(Mutex::new(writer)),
            near_tip,
            checkpointer,
            telemetry,
            wal_path,
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
    pub(crate) fn apply_writer_cache_profile(&self) {
        let writer = self.writer.clone();
        let near_tip = self.near_tip.clone();
        tokio::spawn(async move {
            let kib = if near_tip.load(Ordering::Relaxed) {
                WRITER_CACHE_NEAR_TIP_KIB
            } else {
                WRITER_CACHE_BULK_KIB
            };
            let mut guard = writer.lock().await;
            if let Err(e) = sqlx::query(&format!("PRAGMA cache_size = -{kib}"))
                .execute(&mut *guard)
                .await
            {
                log::warn!("writer cache re-profile to -{kib} KiB failed: {e}");
            }
        });
    }
}

/// Drive WAL checkpoints on a dedicated connection so their copy-into-DB + fsync latency never blocks
/// the confirm writer.
///
/// `wal_checkpoint(PASSIVE)` copies every checkpointable frame into the DB file and, when it reaches
/// them all with no reader still reading the WAL, resets the write pointer to the front so the file is
/// reused instead of growing without bound. PASSIVE is the only mode that takes no exclusive lock: it
/// never blocks a concurrent writer and never hands another connection `SQLITE_BUSY` ("database is
/// locked") — TRUNCATE/RESTART do, which deadlocks the single writer. While a confirm holds an open
/// transaction PASSIVE simply does what it can and resets after the COMMIT lands. The copy+fsync runs
/// here, off the writer, so a slow-fsync backend (iSCSI ~100 ms/sync) never stalls the confirmed peak.
///
/// PASSIVE does not shrink the WAL *file*: it stays at the high-water of the largest single
/// transaction (one batch-confirm window today; a few MB once confirms commit per block). That is a
/// bounded, reused file — harmless for reads now that the coin multi-get point-gets rather than scans.
/// PASSIVE also cannot RESET a WAL some reader still holds a read mark into — which is exactly how
/// the file grows without bound when a pooled reader wedges (the live 1.44 GB WAL): the
/// size-triggered escalation below therefore finishes with TRUNCATE, which (on its own dedicated
/// connection, bounded by `busy_timeout`, retried next tick on SQLITE_BUSY) waits the readers out,
/// resets the log, and shrinks the file to zero — never on the writer's connection, so a busy
/// writer degrades the escalation to "try again next second", not to a confirm stall.
/// Throttle for the size-triggered TRUNCATE escalation.
///
/// TRUNCATE takes the exclusive WAL locks, so every attempt contends with the confirm writer's
/// COMMITs — and while a pooled reader pins the read mark the attempt cannot reset the log at
/// all. Retrying that every tick turns a pinned, over-trigger WAL into a hot loop: one 1–3 s
/// writer-stalling TRUNCATE per second for as long as the reader holds on (observed live during
/// the 9.1M+ era crossing as 1–1.5 s confirm INSERTs interleaved with back-to-back slow
/// checkpoint pragmas). This throttle keeps the FIRST attempt immediate and spaces the retries
/// exponentially (2, 4, … up to 64 ticks) while attempts keep failing to bring the file under
/// the trigger; a successful drain — or the WAL dropping under the trigger on its own — resets
/// it to immediate. PASSIVE draining is not throttled: it stays on its every-tick cadence,
/// so frames keep leaving the WAL between attempts.
struct EscalationBackoff {
    /// Ticks remaining before the next attempt (0 = attempt now).
    delay_ticks: u32,
    /// Width of the current backoff window, doubled per failed attempt.
    current: u32,
}

impl EscalationBackoff {
    const MAX_TICKS: u32 = 64;

    fn new() -> Self {
        Self {
            delay_ticks: 0,
            current: 0,
        }
    }

    /// Whether to run a TRUNCATE this tick. Call once per over-trigger tick; a `false`
    /// consumes one tick of the pending delay.
    fn should_attempt(&mut self) -> bool {
        if self.delay_ticks == 0 {
            true
        } else {
            self.delay_ticks -= 1;
            false
        }
    }

    /// Record an attempt's outcome: `drained` (the file came back under the trigger) resets to
    /// immediate; a failure schedules the next attempt exponentially later.
    fn record(&mut self, drained: bool) {
        if drained {
            *self = Self::new();
        } else {
            self.current = self.current.max(1).saturating_mul(2).min(Self::MAX_TICKS);
            self.delay_ticks = self.current;
        }
    }

    /// The WAL is under the trigger: nothing to escalate, clear any accumulated backoff.
    fn reset(&mut self) {
        *self = Self::new();
    }
}

fn spawn_checkpointer(
    mut conn: SqliteConnection,
    near_tip: Arc<AtomicBool>,
    telemetry: Arc<StoreTelemetry>,
    read: SqlitePool,
    wal_path: PathBuf,
    wal_drain_trigger_bytes: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The pragma's `checkpointed` column is a POSITION in the current WAL (total frames moved so
        // far), not a per-run delta — the counter must add only the advance since the last pass, and
        // treat a smaller value as a WAL reset (write pointer back at the front).
        let mut prev_checkpointed: i64 = 0;
        // Bulk-phase drain cadence. Near the tip the checkpointer drains every tick (1 s); during
        // bulk catch-up it drains far less often so the confirm writer keeps most of the write
        // budget, but it MUST still drain: a from-genesis sync never reaches the near-tip path, so
        // a quiet bulk checkpointer leaves the WAL to grow into the in-writer wal_autocheckpoint
        // failsafe and its blocking copy-to-DB.
        const BULK_CHECKPOINT_TICKS: u32 = 20;
        let mut bulk_tick: u32 = 0;
        let mut escalation = EscalationBackoff::new();
        loop {
            tick.tick().await;
            // Read-pool census every tick: a high idle count while `wal_frames` refuses to fall
            // means a pooled reader holds an old WAL read mark and is blocking the checkpoint reset.
            telemetry
                .read_pool_idle
                .store(read.num_idle() as u64, Ordering::Relaxed);
            telemetry
                .read_pool_size
                .store(u64::from(read.size()), Ordering::Relaxed);
            // The size trigger, checked every tick in every phase (one file-metadata stat): a WAL
            // past the trigger is drained NOW. The cadence alone does not bound the file, since a
            // reader pinning the read mark makes every PASSIVE pass a no-op.
            let over_trigger =
                std::fs::metadata(&wal_path).map_or(0, |m| m.len()) > wal_drain_trigger_bytes;
            // Best-effort: a busy/failed checkpoint is retried on the next tick.
            // Phase-aware cadence: near the tip drain every tick; during bulk drain on the slow cadence
            // above — enough to bound the WAL below the failsafe while leaving the write budget to
            // catch-up — unless the size trigger fired.
            if !near_tip.load(Ordering::Relaxed) {
                bulk_tick = bulk_tick.wrapping_add(1);
                if !over_trigger && !bulk_tick.is_multiple_of(BULK_CHECKPOINT_TICKS) {
                    continue;
                }
            } else {
                bulk_tick = 0;
            }
            checkpoint_pass(&mut conn, &telemetry, &mut prev_checkpointed, "PASSIVE").await;
            // PASSIVE neither resets a reader-pinned log nor shrinks the file, so if it is still
            // past the trigger afterwards, escalate to TRUNCATE. Bounded by the connection's
            // busy_timeout; SQLITE_BUSY lands in telemetry. Retries are BACKED OFF, not
            // every-tick: TRUNCATE contends with the confirm writer, and while a pooled reader
            // pins the read mark a per-tick retry is a hot loop of writer-stalling attempts
            // (see `EscalationBackoff`). PASSIVE draining above is unaffected.
            if !over_trigger {
                escalation.reset();
            } else if std::fs::metadata(&wal_path).map_or(0, |m| m.len()) > wal_drain_trigger_bytes
                && escalation.should_attempt()
            {
                checkpoint_pass(&mut conn, &telemetry, &mut prev_checkpointed, "TRUNCATE").await;
                let drained =
                    std::fs::metadata(&wal_path).map_or(0, |m| m.len()) <= wal_drain_trigger_bytes;
                escalation.record(drained);
            }
        }
    })
}

/// One timed `PRAGMA wal_checkpoint(mode)` on the dedicated checkpointer connection, its result
/// row — (busy, log, checkpointed) per SQLite's wal_checkpoint docs — captured into the
/// telemetry: duration histogram, frames drained, WAL length in frames, busy passes, errors.
async fn checkpoint_pass(
    conn: &mut SqliteConnection,
    telemetry: &StoreTelemetry,
    prev_checkpointed: &mut i64,
    mode: &str,
) {
    let started = std::time::Instant::now();
    let result = sqlx::query(&format!("PRAGMA wal_checkpoint({mode})"))
        .fetch_one(&mut *conn)
        .await;
    match result {
        Ok(row) => {
            telemetry.checkpoint.record(started.elapsed().as_secs_f64());
            let busy: i64 = row.try_get(0).unwrap_or(0);
            let log: i64 = row.try_get(1).unwrap_or(-1);
            let checkpointed: i64 = row.try_get(2).unwrap_or(-1);
            if busy != 0 {
                telemetry
                    .checkpoint_busy_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            if log >= 0 {
                #[allow(clippy::cast_sign_loss)]
                telemetry.wal_frames.store(log as u64, Ordering::Relaxed);
            }
            if checkpointed >= 0 {
                let advance = if checkpointed >= *prev_checkpointed {
                    checkpointed - *prev_checkpointed
                } else {
                    checkpointed // WAL reset: the new position IS this pass's work
                };
                *prev_checkpointed = checkpointed;
                #[allow(clippy::cast_sign_loss)]
                telemetry
                    .wal_frames_checkpointed_total
                    .fetch_add(advance as u64, Ordering::Relaxed);
            }
        }
        Err(_) => {
            telemetry
                .checkpoint_errors_total
                .fetch_add(1, Ordering::Relaxed);
        }
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

#[cfg(test)]
#[path = "../../tests/unit/sqlite/mod/escalation_backoff_tests.rs"]
mod escalation_backoff_tests;
