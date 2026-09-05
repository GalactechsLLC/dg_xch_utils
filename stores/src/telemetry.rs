//! Read-only store telemetry recorded by the backends and rendered by the node's `/metrics`
//! responder: phase-labelled commit latency, and the WAL gauges + checkpoint counters that say
//! whether the WAL is bounded and the checkpointer is keeping up.
//!
//! All plain atomics, recorded on paths that already did the work being measured (the COMMIT and
//! the `wal_checkpoint` pragma).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Default, Debug)]
pub struct OperationMetrics {
    pub calls: AtomicU64,
    pub nanos: AtomicU64,
    pub rows: AtomicU64,
    pub statements: AtomicU64,
}

pub struct OperationTimer {
    metrics: Arc<OperationMetrics>,
    started: Instant,
}

impl Drop for OperationTimer {
    fn drop(&mut self) {
        self.metrics.calls.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .nanos
            .fetch_add(self.started.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

impl OperationMetrics {
    pub fn start(self: &Arc<Self>) -> OperationTimer {
        OperationTimer {
            metrics: self.clone(),
            started: Instant::now(),
        }
    }

    pub fn statement(&self, rows: usize) {
        self.statements.fetch_add(1, Ordering::Relaxed);
        self.rows.fetch_add(rows as u64, Ordering::Relaxed);
    }
}

/// Histogram bucket upper bounds in seconds, shared by the commit and checkpoint histograms.
/// Spans a healthy local-fsync commit (~10 ms) through the ~100 ms/fsync network-storage band up
/// to multi-minute batch-commit and checkpoint stalls; +Inf is implicit (`count`).
pub const DURATION_BUCKETS_SECS: [f64; 12] = [
    0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 60.0, 120.0,
];

/// A Prometheus-shaped cumulative histogram over [`DURATION_BUCKETS_SECS`]: `buckets[i]` counts
/// observations `<= DURATION_BUCKETS_SECS[i]`, `count` is the +Inf bucket, `sum_micros` the total
/// observed time (micros so it stays an atomic integer; the renderer divides out to seconds).
#[derive(Default, Debug)]
pub struct DurationHistogram {
    pub buckets: [AtomicU64; DURATION_BUCKETS_SECS.len()],
    pub count: AtomicU64,
    pub sum_micros: AtomicU64,
}

impl DurationHistogram {
    /// Record one observation. Cumulative semantics: every bucket whose upper bound is `>= seconds`
    /// is incremented, plus the +Inf `count`.
    pub fn record(&self, seconds: f64) {
        for (i, ub) in DURATION_BUCKETS_SECS.iter().enumerate() {
            if seconds <= *ub {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        // Micros as u64: clamp a backwards clock to 0 rather than wrapping.
        let micros = (seconds * 1_000_000.0).max(0.0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        self.sum_micros.fetch_add(micros as u64, Ordering::Relaxed);
    }

    /// Point-in-time copy for rendering (plain numbers, no atomics).
    #[must_use]
    pub fn snapshot(&self) -> HistogramSnapshot {
        let mut buckets = [0u64; DURATION_BUCKETS_SECS.len()];
        for (b, a) in buckets.iter_mut().zip(self.buckets.iter()) {
            *b = a.load(Ordering::Relaxed);
        }
        HistogramSnapshot {
            buckets,
            count: self.count.load(Ordering::Relaxed),
            sum_micros: self.sum_micros.load(Ordering::Relaxed),
        }
    }
}

/// Plain-number histogram state, `PartialEq`/`Default` so render tests stay pure.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HistogramSnapshot {
    pub buckets: [u64; DURATION_BUCKETS_SECS.len()],
    pub count: u64,
    pub sum_micros: u64,
}

/// Telemetry a store backend records as a side effect of work it already does. Held behind an `Arc`
/// shared between the backend and the `/metrics` sampler; see [`crate::BlockStore::telemetry`].
#[derive(Default, Debug)]
pub struct StoreTelemetry {
    pub coin_prepare: Arc<OperationMetrics>,
    pub coin_view: Arc<OperationMetrics>,
    pub coin_prefetch: Arc<OperationMetrics>,
    pub coin_view_reused: AtomicU64,
    pub coin_view_ancestors: AtomicU64,
    pub coin_view_coins: AtomicU64,
    pub coin_prefetch_hits: AtomicU64,
    pub coin_prefetch_fallbacks: AtomicU64,
    pub coin_prepare_input_rows: AtomicU64,
    pub coin_prepare_output_rows: AtomicU64,
    pub writer_wait: Arc<OperationMetrics>,
    pub writer_hold: Arc<OperationMetrics>,
    pub archive_prepare: Arc<OperationMetrics>,
    pub archive_write: Arc<OperationMetrics>,
    pub coin_additions: Arc<OperationMetrics>,
    pub coin_removals: Arc<OperationMetrics>,
    pub coin_lookup: Arc<OperationMetrics>,
    pub hints: Arc<OperationMetrics>,
    pub peak_update: Arc<OperationMetrics>,
    pub prepared_bytes: AtomicU64,
    pub writer_cache_kib: AtomicU64,
    pub cache_hits: AtomicU64,
    pub cache_misses: AtomicU64,
    pub cache_writes: AtomicU64,
    pub cache_spills: AtomicU64,
    /// Writer batch commits (body-append batches AND confirm transactions — every `COMMIT` on the
    /// single writer connection) while the store was in the CATCH-UP band (`near_tip = false`:
    /// one big transaction per sync window).
    pub commit_catch_up: DurationHistogram,
    /// Writer batch commits while in the NEAR-TIP band (`near_tip = true`: one transaction per
    /// confirmed block, active WAL checkpointer).
    pub commit_near_tip: DurationHistogram,
    /// Unix second of the last successful writer COMMIT (0 = none yet this process); read by the
    /// stall dump.
    pub last_commit_unix: AtomicU64,
    /// Completed `wal_checkpoint(PASSIVE)` pragmas on the dedicated checkpointer connection.
    pub checkpoint: DurationHistogram,
    /// WAL frames copied into the main DB by checkpoints (the pragma's `checkpointed` column).
    pub wal_frames_checkpointed_total: AtomicU64,
    /// WAL length in frames as of the last checkpoint (the pragma's `log` column).
    pub wal_frames: AtomicU64,
    /// Checkpoints that returned busy=1 (could not complete a full pass — a reader or the writer
    /// held the WAL). A persistently-busy checkpointer is not keeping up.
    pub checkpoint_busy_total: AtomicU64,
    /// Checkpoint pragmas that failed outright (I/O or lock errors); retries keep these out of the
    /// logs, so a climbing counter here means the WAL is not being drained at all.
    pub checkpoint_errors_total: AtomicU64,
    /// Block-record point reads executed on the read path (`get_block_record`,
    /// `get_block_record_by_height`, and each element of a multi-get).
    pub record_reads: AtomicU64,
    /// Coin-record point reads executed on the read path (`get_coin_record` and each element of
    /// `get_coin_records`), counted separately from the record reads.
    pub coin_reads: AtomicU64,
    /// Read-pool connections currently idle in the pool, sampled by the checkpointer. In WAL mode
    /// an idle pooled reader can hold a read mark at an old WAL position and block the checkpoint
    /// reset, so a high idle count while `wal_frames` refuses to fall names the pinning reader.
    pub read_pool_idle: AtomicU64,
    /// Read-pool total connections (idle + in-use), sampled by the checkpointer.
    pub read_pool_size: AtomicU64,
}

impl StoreTelemetry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cfg(test)]
#[path = "../tests/unit/telemetry/tests.rs"]
mod tests;
