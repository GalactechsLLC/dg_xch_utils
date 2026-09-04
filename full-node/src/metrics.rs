mod memory;
#[cfg(feature = "profiling")]
pub(crate) mod profiling;
mod rendering;

pub use memory::log_startup_memory;
use memory::{
    jemalloc_stat_active, jemalloc_stat_allocated, jemalloc_stat_resident, jemalloc_stat_retained,
    process_rss_bytes,
};
pub use rendering::render_metrics;

use dg_xch_core::protocols::PeerMap;
use dg_xch_node::{Mempool, SyncMetrics};
use dg_xch_p2p::{NetCounters, PeerRegistry};
use dg_xch_stores::{BlockStore, DURATION_BUCKETS_SECS, HistogramSnapshot};
use log::{info, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(feature = "profiling")]
use std::time::Duration;

// Sampling-profiler window for a /debug/flamegraph request, and the hard cap on the whole profile (sample +
// symbolize + SVG render) so a wedged profiler can never hold the connection forever.
#[cfg(feature = "profiling")]
pub(crate) const FLAMEGRAPH_SECONDS: u64 = 20;
#[cfg(feature = "profiling")]
const FLAMEGRAPH_HZ: i32 = 97; // prime-ish sample rate; avoids lockstep with periodic node work
#[cfg(feature = "profiling")]
pub(crate) const FLAMEGRAPH_TIMEOUT: Duration = Duration::from_secs(FLAMEGRAPH_SECONDS + 20);
// Hard cap on a /debug/heap dump (jemalloc prof.dump writes the file synchronously; a wedged disk
// must not hold the connection forever).
#[cfg(feature = "profiling")]
pub(crate) const HEAP_DUMP_TIMEOUT: Duration = Duration::from_secs(30);

/// RED-style block-producer pipeline counters. The linear
/// stage counts are cheap atomics; the drop-reason / validate-result / broadcast-peer-type fans are
/// labelled `Mutex<HashMap>`s (bounded enums — safe as Prometheus labels, unlike the high-cardinality
/// quality strings, which stay in the log events). Held behind an `Arc`, shared by the read-loop
/// `StoreApi` (declare/build/signed_values) and the driver (`process_ub_inbox` + the two broadcasts);
/// zero-cost when nobody scrapes.
#[derive(Default)]
pub struct ProducerMetrics {
    /// S1 — a `DeclareProofOfSpace` arrived (distinguishes never-received from received-then-dropped).
    pub declares_received: AtomicU64,
    /// S3 — a candidate unfinished block was assembled from an accepted proof.
    pub candidates_built: AtomicU64,
    /// S4 — a `RequestSignedValues` was returned to the farmer to sign.
    pub request_signed_values_sent: AtomicU64,
    /// S5 — a `SignedValues` reply was received from the farmer.
    pub signed_values_received: AtomicU64,
    /// S5 — foliage signatures spliced into a finished unfinished block.
    pub ub_assembled: AtomicU64,
    /// S8 — a full block confirmed whose header hash matches one of OUR farmed unfinished blocks.
    pub full_block_added: AtomicU64,
    // S1/S2 `declares_validated{result}`: accepted | unknown_signage_point | stale_signage_point |
    // unknown_sub_slot | pospace_verify_fail | not_synced.
    validated: std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
    // `candidates_dropped{reason}`: the full producer drop taxonomy.
    dropped: std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
    // `candidates_requeued{reason}`: a candidate that HIT a transient wall (e.g. a store error resolving
    // its parent) and was put BACK on the inbox for a retry — NOT lost. Kept separate from `dropped` so a
    // preserved candidate can never inflate the loss taxonomy the `ub_prev_unknown` quantification reads.
    requeued: std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
    // S7/S7t `ub_broadcast{peer_type}`: full_node | timelord.
    ub_broadcast: std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
}

impl ProducerMetrics {
    pub fn declare_received(&self) {
        self.declares_received.fetch_add(1, Ordering::Relaxed);
    }
    pub fn validated(&self, result: &'static str) {
        producer_bump(&self.validated, result);
    }
    pub fn candidate_built(&self) {
        self.candidates_built.fetch_add(1, Ordering::Relaxed);
    }
    pub fn candidate_dropped(&self, reason: &'static str) {
        producer_bump(&self.dropped, reason);
    }
    /// A candidate hit a transient, retryable wall and was re-queued (kept), not dropped.
    pub fn candidate_requeued(&self, reason: &'static str) {
        producer_bump(&self.requeued, reason);
    }
    /// Test/inspection accessor: how many drops were recorded under `reason`.
    #[must_use]
    pub fn dropped_count(&self, reason: &str) -> u64 {
        self.dropped
            .lock()
            .expect("producer counter lock")
            .get(reason)
            .copied()
            .unwrap_or(0)
    }
    /// Test/inspection accessor: how many re-queues were recorded under `reason`.
    #[must_use]
    pub fn requeued_count(&self, reason: &str) -> u64 {
        self.requeued
            .lock()
            .expect("producer counter lock")
            .get(reason)
            .copied()
            .unwrap_or(0)
    }
    pub fn request_signed_values(&self) {
        self.request_signed_values_sent
            .fetch_add(1, Ordering::Relaxed);
    }
    pub fn signed_values(&self) {
        self.signed_values_received.fetch_add(1, Ordering::Relaxed);
    }
    pub fn ub_assembled(&self) {
        self.ub_assembled.fetch_add(1, Ordering::Relaxed);
    }
    pub fn ub_broadcast(&self, peer_type: &'static str) {
        producer_bump(&self.ub_broadcast, peer_type);
    }
    pub fn full_block(&self) {
        self.full_block_added.fetch_add(1, Ordering::Relaxed);
    }
    fn validated_counts(&self) -> Vec<(&'static str, u64)> {
        sorted_counts(&self.validated)
    }
    fn dropped_counts(&self) -> Vec<(&'static str, u64)> {
        sorted_counts(&self.dropped)
    }
    fn requeued_counts(&self) -> Vec<(&'static str, u64)> {
        sorted_counts(&self.requeued)
    }
    fn ub_broadcast_counts(&self) -> Vec<(&'static str, u64)> {
        sorted_counts(&self.ub_broadcast)
    }
}

fn producer_bump(
    map: &std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
    key: &'static str,
) {
    *map.lock()
        .expect("producer counter lock")
        .entry(key)
        .or_insert(0) += 1;
}

/// The read handles the `/metrics` responder samples on each scrape. Cheap clones (Arc + atomics).
#[derive(Clone)]
pub struct MetricsSources<S> {
    pub store: Arc<S>,
    pub metrics: Arc<SyncMetrics>,
    pub claimed_peak: Arc<AtomicU32>,
    pub registry: Arc<PeerRegistry>,
    // The --sync-from anchor height (0 = genesis node); a constant gauge so dashboards
    // can chart each leg against its own span.
    pub sync_from: u32,
    // The peer server's shared inbound connection map (cert-hash id -> live SocketPeer). Gauged
    // by length so a live retention bisect can NAME this collection if it climbs — the
    // retention instrument for the inbound-session retainer the PeerRegistry-derived counters never saw
    // (the server admits connections straight into this map, bypassing admit_inbound).
    pub inbound_peers: PeerMap,
    // Peer-link traffic counters (shared with every handler map + the broadcast paths).
    pub net: Arc<NetCounters>,
    // The mempool, for the size/cost gauges.
    pub mempool: Arc<tokio::sync::Mutex<Mempool>>,
    // Signage-point telemetry (latest accepted index / running total).
    pub sp_current_index: Arc<AtomicU32>,
    pub signage_points_total: Arc<std::sync::atomic::AtomicU64>,
    // Sync-liveness witnesses for the `/health` liveness probe. Progress is recorded on every
    // scrape of EITHER endpoint (see `sample`), so the stall clock is fresh independent of who polls.
    pub health: Arc<HealthState>,
    // Block-producer pipeline counters (the first-block funnel).
    pub producer: Arc<ProducerMetrics>,
    // Unix second the current follow/backtrack request went in flight (0 = idle), set/cleared by the
    // server's follow paths - the stall dump's "what was the node waiting on" witness. During the
    // 7-minute silent wedge we could only infer the hung request from last-span timestamps; this
    // names its age directly.
    pub follow_inflight_since: Arc<AtomicU64>,
}

// Sync-liveness thresholds for the `/health` liveness probe.
//
// STALL_SECS: mainnet infuses a block roughly every 18.75s, so a healthy synced node's confirmed
// peak advances far inside this window; a node BELOW tip, WITH peers, whose confirmed peak has
// not moved for this long is the silent-stall signature (e.g. a worker-thread panic that leaves
// the process alive but sync dead). 300s = ~16 expected blocks of no progress — comfortably past
// transient peer churn / one slow reorg.
const STALL_SECS: u64 = 300;
// Boot grace: a fresh node must dial peers, fetch a ~14 MB weight proof, and run the multi-minute proof
// verify before its first fast-sync peak lands. The probe cannot fail inside this window regardless of
// progress — it covers the cold from-zero start where peak legitimately sits at 0 for a while.
const BOOT_GRACE_SECS: u64 = 120;
const CONFIRM_MAX_SECS: u64 = 300;

// Seconds since the Unix epoch (0 on a clock before the epoch — never panics).
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Progress witnesses for the `/health` liveness probe. All atomics, no lock — read/written on the
/// metrics accept loop and updated on every scrape of EITHER `/metrics` or `/health` (see
/// [`MetricsSources::sample`]), so the stall clock stays fresh regardless of which endpoint the
/// kubelet or Prometheus polls. Cheap to clone (held behind an `Arc`).
#[derive(Debug)]
pub struct HealthState {
    // Wall-clock second the process started — anchors the boot grace window.
    boot_unix: u64,
    // Highest confirmed peak height observed so far, and the second it last increased. The peak is the
    // primary sync-progress signal (mainnet infuses ~every 18.75s, so it moves on any healthy node).
    last_peak: AtomicU64,
    last_progress_unix: AtomicU64,
    // Highest `blocks_downloaded` counter observed — a SECONDARY witness so the multi-minute fast-sync
    // BODY-download phase (confirmed peak does not move until the whole window lands) still counts as
    // progress. The one window this cannot cover is a pure CPU weight-proof verify with no I/O; the boot
    // grace covers it at cold start, and mid-run it is rare (only after falling far behind) — see caveats.
    last_downloaded: AtomicU64,
    // Debounce for the stall-dump log line: one structured dump per stall EPISODE (set on the first
    // 503, cleared on the next 200), so a persisting stall does not spam a dump per kubelet poll.
    stall_dumped: AtomicBool,
}

struct Health {
    status: &'static str,
    body: String,
}

impl Health {
    fn ok(reason: &str) -> Self {
        Self {
            status: "200 OK",
            body: format!("ok: {reason}\n"),
        }
    }
    fn stalled(since: u64, peers: u64) -> Self {
        Self {
            status: "503 Service Unavailable",
            body: format!("stalled: no peak advance in {since}s (below tip, {peers} peers)\n"),
        }
    }
}

impl HealthState {
    // Construct with an explicit boot second (the test seam); `new` supplies the live clock.
    fn new_at(now: u64) -> Arc<Self> {
        Arc::new(Self {
            boot_unix: now,
            last_peak: AtomicU64::new(0),
            // Seed the progress clock to boot so a node that never advances still gets the full grace
            // window before the first possible failure (rather than reading stalled from second one).
            last_progress_unix: AtomicU64::new(now),
            last_downloaded: AtomicU64::new(0),
            stall_dumped: AtomicBool::new(false),
        })
    }

    // True exactly once per stall episode: the first 503 wins the dump, subsequent 503 polls skip it.
    fn should_dump_stall(&self) -> bool {
        !self.stall_dumped.swap(true, Ordering::Relaxed)
    }

    fn clear_stall(&self) {
        self.stall_dumped.store(false, Ordering::Relaxed);
    }

    // Seconds since the last recorded progress (peak advance or download climb).
    fn progress_age(&self, now: u64) -> u64 {
        now.saturating_sub(self.last_progress_unix.load(Ordering::Relaxed))
    }

    /// A fresh `HealthState` anchored at the current wall clock.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::new_at(unix_now())
    }

    // Record forward progress from a fresh sample: bump the progress clock when EITHER the confirmed
    // peak advanced or the download counter climbed. Lock-free and monotonic — two concurrent scrapers
    // race only to write the same-or-newer `now`, which is harmless.
    fn observe(&self, peak: u64, downloaded: u64, now: u64) {
        let peak_advanced = peak > self.last_peak.fetch_max(peak, Ordering::Relaxed);
        let dl_advanced = downloaded
            > self
                .last_downloaded
                .fetch_max(downloaded, Ordering::Relaxed);
        if peak_advanced || dl_advanced {
            self.last_progress_unix.store(now, Ordering::Relaxed);
        }
    }

    fn verdict(&self, snap: &MetricsSnapshot, now: u64, follow_inflight_since: u64) -> Health {
        if now.saturating_sub(self.boot_unix) < BOOT_GRACE_SECS {
            return Health::ok("boot grace");
        }
        if snap.peak_height > 0 && snap.peak_height >= snap.claimed_peak {
            return Health::ok("caught up to peer tip");
        }
        if snap.outbound_peers == 0 {
            return Health::ok("no outbound peers to sync from");
        }
        let since = now.saturating_sub(self.last_progress_unix.load(Ordering::Relaxed));
        if since <= STALL_SECS {
            return Health::ok("advancing");
        }
        // Past the stall window with no witnessed peak advance or download climb — BUT a window-batched
        // confirm actively in flight IS progress: it freezes both witnesses (the confirmed peak jumps its
        // whole window only after the store commit; blocks_downloaded already peaked before the write) while
        // the node is correctly committing. `follow_inflight_since` (0 = idle) is set for the entire
        // drain+confirm window on every backend, so it is the correct, backend-agnostic liveness signal on
        // the Postgres/SAN catch-up path where last_commit_unix is None. Bounded by CONFIRM_MAX_SECS so a
        // genuinely wedged/deadlocked writer still eventually 503s (anti-unkillable). Monotonic-safe:
        // saturating_sub yields 0 if the set-time reads ahead of `now` under clock skew (favor healthy while
        // freshly set).
        if follow_inflight_since != 0
            && now.saturating_sub(follow_inflight_since) <= CONFIRM_MAX_SECS
        {
            return Health::ok("confirm in flight");
        }
        Health::stalled(since, snap.outbound_peers)
    }
}

/// Store-side point-in-time state (sqlite today; `None` for backends that record no telemetry, so
/// the renderer skips the series instead of exporting misleading zeros).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StoreSnapshot {
    // The bounded-WAL witness: current `-wal` file size in bytes.
    pub wal_bytes: u64,
    // 1 = near-tip band (per-block commits + active checkpointer), 0 = catch-up band. Doubles as the
    // dashboard's band-shading series and the "is the checkpointer gated on" gauge.
    pub near_tip: u64,
    pub commit_catch_up: HistogramSnapshot,
    pub commit_near_tip: HistogramSnapshot,
    pub checkpoint: HistogramSnapshot,
    pub wal_frames: u64,
    pub wal_frames_checkpointed_total: u64,
    pub checkpoint_busy_total: u64,
    pub checkpoint_errors_total: u64,
    // Read-path point-read counters (block records / coin records): the staging-residue
    // attribution split — rate(record_reads) against the window cadence names the per-block
    // read serialization; rate(coin_reads) is the confirmed-set validation volume.
    pub record_reads: u64,
    pub coin_reads: u64,
    pub read_pool_idle: u64,
    pub read_pool_size: u64,
}

// A point-in-time sample rendered to Prometheus text. Plain numbers so the render is pure + unit-testable.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub peak_height: u64,
    pub claimed_peak: u64,
    // claimed − confirmed, floored at 0 — THE at-tip/catching-up/stalled health number. 0 also when
    // no peer has announced a tip yet (claimed 0): read it alongside the peer gauges.
    pub tip_lag: u64,
    // The local sync FLOOR: lowest confirmed main-chain height, read from the STORE's truth (an
    // era-anchored node reports its backfilled anchor floor, not its --sync-from CLI arg). None on
    // an empty store — rendered absent, never as a fake 0 that would read as "genesis-synced".
    // Left edge of the dashboard sync-progress bar: synced = [base .. peak], remaining = [peak .. tip].
    pub sync_base_height: Option<u64>,
    pub blocks_downloaded: u64,
    pub blocks_confirmed: u64,
    pub reclaimed: u64,
    pub peak_window: u64,
    pub peak_inflight_blocks: u64,
    pub rss_bytes: u64,
    pub outbound_peers: u64,
    pub inbound_peers: u64,
    // Live entries in the peer server's inbound connection map. Unlike `inbound_peers` (derived
    // from PeerRegistry, which the server path never populates), this is the TRUE resident inbound
    // session count — the second-retainer witness: a monotonic climb here names the leak.
    pub inbound_connections: u64,
    pub window_vdf_micros: u64,
    pub window_sig_micros: u64,
    pub window_body_micros: u64,
    // The sequential staging-loop wall (per-block store reads + record derivation) — the
    // phase between body precompute and VDF drain.
    pub window_stage_micros: u64,
    pub window_confirm_micros: u64,
    pub window_blocks: u64,
    pub window_tx_blocks: u64,
    // Cross-window body pipeline: bodies the driver handed in precomputed, and the driver's
    // join wait on that precompute before the window could start.
    pub window_body_provided: u64,
    pub window_pre_wait_micros: u64,
    // Stage-ahead pipeline: how long the confirm waited on the previous window's spawned drain.
    pub window_drain_wait_micros: u64,
    // jemalloc's own view: `allocated` = live bytes the program holds; `resident` =
    // pages jemalloc keeps from the OS. resident >> allocated = allocator holdback;
    // allocated climbing = true retention. All 0 when jemalloc isn't the global allocator.
    // `active` (pages backing allocations) and `retained` (unmapped-but-kept VM) complete the RSS
    // attribution: RSS ≈ resident; resident − active = fragmentation/holdback; retained = the
    // allocator's kept-back reserve that never shows in RSS.
    pub alloc_allocated: u64,
    pub alloc_active: u64,
    pub alloc_resident: u64,
    pub alloc_retained: u64,
    // Engine collection sizes (retention bisect): pending orphans is the unbounded suspect.
    pub engine_cache_records: u64,
    pub engine_pending_orphans: u64,
    pub engine_staged_generators: u64,
    // Difficulty-window record serving.
    pub difficulty_window_cache_hits: u64,
    pub difficulty_window_store_reads: u64,
    // Follow-pipeline idle attribution + window readahead: cumulative fetch-wait vs
    // whole-step micros (idle fraction = rate(fetch_wait)/rate(step)), the adaptive depth K,
    // windows in flight, and the hit/miss counters.
    pub follow_fetch_wait_micros: u64,
    pub follow_step_micros: u64,
    pub readahead_depth: u64,
    pub readahead_inflight: u64,
    pub readahead_hits: u64,
    pub readahead_misses: u64,
    // Block queue: resident PRESENT bytes in the reorder buffer and slots held ahead of the
    // consumer — the prefetch share of live allocation, so runtime growth can be read with the
    // queue subtracted.
    pub queue_resident_bytes: u64,
    pub queue_len: u64,
    // The highest fresh OUTBOUND peer claim — the servable fetch frontier the follow producer
    // clamps to. 0 = no live outbound claim. A tip pinned at-or-below the local peak while
    // claimed runs ahead is the silent-idle wedge signature.
    pub outbound_tip: u64,
    pub sync_from: u64,
    // Per-message-type traffic counters, sorted by label for a stable render.
    pub messages_in: Vec<(&'static str, u64)>,
    pub messages_out: Vec<(&'static str, u64)>,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub mempool_size: u64,
    pub mempool_cost: u64,
    pub mempool_max_cost: u64,
    pub sp_current_index: u64,
    pub signage_points_total: u64,
    pub last_reorg_depth: u64,
    // Block-producer pipeline. The scalar stage counts plus the
    // three labelled fans (validate result, drop reason, broadcast peer type).
    pub producer_declares_received: u64,
    pub producer_candidates_built: u64,
    pub producer_request_signed_values: u64,
    pub producer_signed_values_received: u64,
    pub producer_ub_assembled: u64,
    pub producer_full_blocks: u64,
    pub producer_validated: Vec<(&'static str, u64)>,
    pub producer_candidates_dropped: Vec<(&'static str, u64)>,
    pub producer_candidates_requeued: Vec<(&'static str, u64)>,
    pub producer_ub_broadcast: Vec<(&'static str, u64)>,
    // Store commit/WAL/checkpoint telemetry; None when the backend records none (mmap, postgres).
    pub store: Option<StoreSnapshot>,
}

impl<S: BlockStore + Send + Sync> MetricsSources<S> {
    /// The `/metrics` body — same renderer the accept-loop responder uses.
    pub async fn metrics_text(&self) -> String {
        render_metrics(&self.sample().await)
    }

    pub async fn health_check(&self) -> (&'static str, String) {
        let now = unix_now();
        let snap = self.sample_liveness().await;
        let follow_inflight_since = self.follow_inflight_since.load(Ordering::Relaxed);
        let health = self.health.verdict(&snap, now, follow_inflight_since);
        if health.status.starts_with("503") {
            if self.health.should_dump_stall() {
                self.log_stall_dump(&snap, now);
            }
        } else {
            self.health.clear_stall();
        }
        (health.status, health.body)
    }

    async fn sample(&self) -> MetricsSnapshot {
        let peak_height = self
            .store
            .get_peak()
            .await
            .ok()
            .flatten()
            .map_or(0, |(_, h)| u64::from(h));
        let (mempool_size, mempool_cost, mempool_max_cost) = {
            let mp = self.mempool.lock().await;
            (mp.len() as u64, mp.total_cost(), mp.max_total_cost())
        };
        let m = &self.metrics;
        let blocks_downloaded = m.blocks_downloaded.load(Ordering::Relaxed);
        // Record sync progress on every scrape of either endpoint, so the /health stall clock is kept
        // fresh by whoever polls (kubelet on /health, Prometheus on /metrics) — no driver hot-loop edit.
        self.health
            .observe(peak_height, blocks_downloaded, unix_now());
        let claimed_peak = u64::from(self.claimed_peak.load(Ordering::Relaxed));
        // Store telemetry snapshot (sqlite only today): cheap atomic loads plus one WAL-file
        // metadata stat per scrape.
        let store = self.store.telemetry().map(|t| StoreSnapshot {
            wal_bytes: self.store.wal_bytes(),
            near_tip: u64::from(self.store.near_tip()),
            commit_catch_up: t.commit_catch_up.snapshot(),
            commit_near_tip: t.commit_near_tip.snapshot(),
            checkpoint: t.checkpoint.snapshot(),
            wal_frames: t.wal_frames.load(Ordering::Relaxed),
            wal_frames_checkpointed_total: t.wal_frames_checkpointed_total.load(Ordering::Relaxed),
            checkpoint_busy_total: t.checkpoint_busy_total.load(Ordering::Relaxed),
            checkpoint_errors_total: t.checkpoint_errors_total.load(Ordering::Relaxed),
            record_reads: t.record_reads.load(Ordering::Relaxed),
            coin_reads: t.coin_reads.load(Ordering::Relaxed),
            read_pool_idle: t.read_pool_idle.load(Ordering::Relaxed),
            read_pool_size: t.read_pool_size.load(Ordering::Relaxed),
        });
        MetricsSnapshot {
            peak_height,
            claimed_peak,
            tip_lag: claimed_peak.saturating_sub(peak_height),
            sync_base_height: self
                .store
                .min_record_height()
                .await
                .ok()
                .flatten()
                .map(u64::from),
            store,
            blocks_downloaded,
            blocks_confirmed: m.blocks_confirmed.load(Ordering::Relaxed),
            reclaimed: m.reclaimed.load(Ordering::Relaxed),
            peak_window: m.peak_window.load(Ordering::Relaxed) as u64,
            peak_inflight_blocks: m.peak_inflight_blocks.load(Ordering::Relaxed) as u64,
            rss_bytes: process_rss_bytes(),
            outbound_peers: self.registry.outbound_count().await as u64,
            inbound_peers: self.registry.inbound_count().await as u64,
            inbound_connections: self.inbound_peers.read().await.len() as u64,
            window_vdf_micros: m.window_vdf_micros.load(Ordering::Relaxed),
            window_sig_micros: m.window_sig_micros.load(Ordering::Relaxed),
            window_body_micros: m.window_body_micros.load(Ordering::Relaxed),
            window_stage_micros: m.window_stage_micros.load(Ordering::Relaxed),
            window_confirm_micros: m.window_confirm_micros.load(Ordering::Relaxed),
            window_blocks: m.window_blocks.load(Ordering::Relaxed),
            window_tx_blocks: m.window_tx_blocks.load(Ordering::Relaxed),
            window_body_provided: m.window_body_provided.load(Ordering::Relaxed),
            window_pre_wait_micros: m.window_pre_wait_micros.load(Ordering::Relaxed),
            window_drain_wait_micros: m.window_drain_wait_micros.load(Ordering::Relaxed),
            alloc_allocated: jemalloc_stat_allocated(),
            alloc_active: jemalloc_stat_active(),
            alloc_resident: jemalloc_stat_resident(),
            alloc_retained: jemalloc_stat_retained(),
            engine_cache_records: m.engine_cache_records.load(Ordering::Relaxed),
            engine_pending_orphans: m.engine_pending_orphans.load(Ordering::Relaxed),
            engine_staged_generators: m.engine_staged_generators.load(Ordering::Relaxed),
            difficulty_window_cache_hits: m.difficulty_window_cache_hits.load(Ordering::Relaxed),
            difficulty_window_store_reads: m.difficulty_window_store_reads.load(Ordering::Relaxed),
            follow_fetch_wait_micros: m.follow_fetch_wait_micros.load(Ordering::Relaxed),
            follow_step_micros: m.follow_step_micros.load(Ordering::Relaxed),
            readahead_depth: m.readahead_depth.load(Ordering::Relaxed),
            readahead_inflight: m.readahead_inflight.load(Ordering::Relaxed),
            readahead_hits: m.readahead_hits.load(Ordering::Relaxed),
            readahead_misses: m.readahead_misses.load(Ordering::Relaxed),
            queue_resident_bytes: m.queue_resident_bytes.load(Ordering::Relaxed),
            queue_len: m.queue_len.load(Ordering::Relaxed),
            outbound_tip: m.outbound_tip.load(Ordering::Relaxed),
            sync_from: u64::from(self.sync_from),
            messages_in: sorted_counts(&self.net.messages_in),
            messages_out: sorted_counts(&self.net.messages_out),
            bytes_in: self.net.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.net.bytes_out.load(Ordering::Relaxed),
            mempool_size,
            mempool_cost,
            mempool_max_cost,
            sp_current_index: u64::from(self.sp_current_index.load(Ordering::Relaxed)),
            signage_points_total: self
                .signage_points_total
                .load(std::sync::atomic::Ordering::Relaxed),
            last_reorg_depth: m.last_reorg_depth.load(Ordering::Relaxed),
            producer_declares_received: self.producer.declares_received.load(Ordering::Relaxed),
            producer_candidates_built: self.producer.candidates_built.load(Ordering::Relaxed),
            producer_request_signed_values: self
                .producer
                .request_signed_values_sent
                .load(Ordering::Relaxed),
            producer_signed_values_received: self
                .producer
                .signed_values_received
                .load(Ordering::Relaxed),
            producer_ub_assembled: self.producer.ub_assembled.load(Ordering::Relaxed),
            producer_full_blocks: self.producer.full_block_added.load(Ordering::Relaxed),
            producer_validated: self.producer.validated_counts(),
            producer_candidates_dropped: self.producer.dropped_counts(),
            producer_candidates_requeued: self.producer.requeued_counts(),
            producer_ub_broadcast: self.producer.ub_broadcast_counts(),
        }
    }
}

impl<S: BlockStore + Send + Sync> MetricsSources<S> {
    pub(crate) async fn sample_liveness(&self) -> MetricsSnapshot {
        let peak_height = self
            .store
            .get_peak()
            .await
            .ok()
            .flatten()
            .map_or(0, |(_, h)| u64::from(h));
        let blocks_downloaded = self.metrics.blocks_downloaded.load(Ordering::Relaxed);
        self.health
            .observe(peak_height, blocks_downloaded, unix_now());
        MetricsSnapshot {
            peak_height,
            claimed_peak: u64::from(self.claimed_peak.load(Ordering::Relaxed)),
            outbound_peers: self.registry.outbound_count().await as u64,
            ..Default::default()
        }
    }

    // The stall dump: ONE structured line, logged when /health first reports 503 in an episode,
    // recording what the node was last doing. Ages are seconds; -1 = never/idle this process.
    fn log_stall_dump(&self, snap: &MetricsSnapshot, now: u64) {
        let age = |unix: u64| -> i64 {
            if unix == 0 {
                -1
            } else {
                i64::try_from(now.saturating_sub(unix)).unwrap_or(i64::MAX)
            }
        };
        let last_commit_unix = self
            .store
            .telemetry()
            .map_or(0, |t| t.last_commit_unix.load(Ordering::Relaxed));
        warn!(
            "sync stalled — self-report of last activity (one line per stall episode) event={} peak_height={} claimed_peak={} tip_lag={} outbound_peers={} last_progress_age_secs={} last_commit_age_secs={} follow_inflight_age_secs={} wal_bytes={}",
            "fullnode.stall.dump",
            snap.peak_height,
            snap.claimed_peak,
            snap.claimed_peak.saturating_sub(snap.peak_height),
            snap.outbound_peers,
            self.health.progress_age(now),
            age(last_commit_unix),
            age(self.follow_inflight_since.load(Ordering::Relaxed)),
            self.store.wal_bytes()
        );
    }
}

// Snapshot a per-type counter map into a label-sorted vec (stable Prometheus output).
fn sorted_counts(
    map: &std::sync::Mutex<std::collections::HashMap<&'static str, u64>>,
) -> Vec<(&'static str, u64)> {
    let mut v: Vec<_> = map
        .lock()
        .expect("net counter lock")
        .iter()
        .map(|(k, n)| (*k, *n))
        .collect();
    v.sort_unstable();
    v
}

#[cfg(test)]
#[path = "../tests/unit/metrics.rs"]
mod tests;
