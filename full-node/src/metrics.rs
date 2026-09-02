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
    // daemon's follow paths — the stall dump's "what was the node waiting on" witness. During the
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

// Render one histogram family in Prometheus text format: for each (label, snapshot) pair a full
// cumulative bucket ladder + the implicit +Inf, then _sum (seconds) and _count. An empty label
// string renders the bare series (no `{phase=...}`).
fn render_histogram(
    out: &mut String,
    name: &str,
    help: &str,
    series: &[(&str, &HistogramSnapshot)],
) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} histogram\n"));
    for (phase, h) in series {
        let label = |le: &str| {
            if phase.is_empty() {
                format!("{{le=\"{le}\"}}")
            } else {
                format!("{{phase=\"{phase}\",le=\"{le}\"}}")
            }
        };
        for (i, ub) in DURATION_BUCKETS_SECS.iter().enumerate() {
            out.push_str(&format!(
                "{name}_bucket{} {}\n",
                label(&format!("{ub}")),
                h.buckets[i]
            ));
        }
        out.push_str(&format!("{name}_bucket{} {}\n", label("+Inf"), h.count));
        let suffix = if phase.is_empty() {
            String::new()
        } else {
            format!("{{phase=\"{phase}\"}}")
        };
        #[allow(clippy::cast_precision_loss)]
        let sum_secs = h.sum_micros as f64 / 1_000_000.0;
        out.push_str(&format!("{name}_sum{suffix} {sum_secs}\n"));
        out.push_str(&format!("{name}_count{suffix} {}\n", h.count));
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

/// Render a snapshot to Prometheus text-exposition format (v0.0.4). Every series is a gauge/counter with a
/// HELP + TYPE line; the whole node's live sync state in one scrape.
#[must_use]
pub fn render_metrics(s: &MetricsSnapshot) -> String {
    let mut out = String::with_capacity(1024);
    let g = |out: &mut String, name: &str, help: &str, kind: &str, val: u64| {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} {kind}\n"));
        out.push_str(&format!("{name} {val}\n"));
    };
    g(
        &mut out,
        "fullnode_peak_height",
        "Confirmed local peak height.",
        "gauge",
        s.peak_height,
    );
    g(
        &mut out,
        "fullnode_claimed_peak_height",
        "Highest peer-announced peak height.",
        "gauge",
        s.claimed_peak,
    );
    g(
        &mut out,
        "fullnode_tip_lag",
        "Blocks behind the best peer-announced tip (claimed - confirmed, floored at 0; also 0 before any peer announces - read with the peer gauges).",
        "gauge",
        s.tip_lag,
    );
    // Absent (not 0) on an empty store: 0 must always mean "genesis-synced".
    if let Some(base) = s.sync_base_height {
        g(
            &mut out,
            "fullnode_sync_base_height",
            "Lowest confirmed main-chain height in the local store (0 = genesis-synced; an era-anchored node reports its backfilled floor). Left edge of the sync-progress bar.",
            "gauge",
            base,
        );
    }
    g(
        &mut out,
        "fullnode_blocks_downloaded_total",
        "Block bodies downloaded by the sync pipeline.",
        "counter",
        s.blocks_downloaded,
    );
    g(
        &mut out,
        "fullnode_blocks_confirmed_total",
        "Blocks validated and confirmed into the store.",
        "counter",
        s.blocks_confirmed,
    );
    g(
        &mut out,
        "fullnode_reservations_reclaimed_total",
        "Reservation windows reclaimed from stalled peers.",
        "counter",
        s.reclaimed,
    );
    g(
        &mut out,
        "fullnode_peak_reservation_window",
        "Peak in-flight reservation-window identifiers.",
        "gauge",
        s.peak_window,
    );
    g(
        &mut out,
        "fullnode_peak_inflight_blocks",
        "Peak simultaneously-resident downloaded blocks.",
        "gauge",
        s.peak_inflight_blocks,
    );
    g(
        &mut out,
        "fullnode_process_resident_bytes",
        "Process resident set size in bytes.",
        "gauge",
        s.rss_bytes,
    );
    g(
        &mut out,
        "fullnode_outbound_peers",
        "Live outbound peer connections.",
        "gauge",
        s.outbound_peers,
    );
    g(
        &mut out,
        "fullnode_inbound_peers",
        "Live inbound peer connections.",
        "gauge",
        s.inbound_peers,
    );
    g(
        &mut out,
        "fullnode_inbound_connections",
        "Resident entries in the peer server's inbound connection map (true inbound session count; a monotonic climb is the retention witness).",
        "gauge",
        s.inbound_connections,
    );
    g(
        &mut out,
        "fullnode_window_vdf_micros",
        "Last sync window VDF-drain wall time in microseconds.",
        "gauge",
        s.window_vdf_micros,
    );
    g(
        &mut out,
        "fullnode_window_sig_micros",
        "Last sync window header-signature-drain wall time in microseconds.",
        "gauge",
        s.window_sig_micros,
    );
    g(
        &mut out,
        "fullnode_window_body_provided",
        "Bodies of the last sync window handed in precomputed by the driver's cross-window pipeline (the rest ran inline in window.body).",
        "gauge",
        s.window_body_provided,
    );
    g(
        &mut out,
        "fullnode_window_pre_wait_micros",
        "Microseconds the driver waited joining the previous window's body precompute before starting the last window (0 = precompute finished in time or none ran).",
        "gauge",
        s.window_pre_wait_micros,
    );
    g(
        &mut out,
        "fullnode_window_drain_wait_micros",
        "Microseconds the confirm waited on the previous window's spawned vdf/sig drain (the stage-ahead pipeline's backpressure signal).",
        "gauge",
        s.window_drain_wait_micros,
    );
    g(
        &mut out,
        "fullnode_window_body_micros",
        "Last sync window parallel body-precompute wall time in microseconds.",
        "gauge",
        s.window_body_micros,
    );
    g(
        &mut out,
        "fullnode_window_stage_micros",
        "Last sync window sequential staging-loop wall time in microseconds (per-block store reads + record derivation) — the residue phase between body precompute and VDF drain.",
        "gauge",
        s.window_stage_micros,
    );
    g(
        &mut out,
        "fullnode_window_confirm_micros",
        "Last sync window batched-confirm wall time in microseconds.",
        "gauge",
        s.window_confirm_micros,
    );
    g(
        &mut out,
        "fullnode_follow_fetch_wait_micros_total",
        "Cumulative microseconds the follow driver waited on the network for its next window (readahead take + direct-fetch fallback). Validator idle fraction = rate of this over rate(fullnode_follow_step_micros_total).",
        "counter",
        s.follow_fetch_wait_micros,
    );
    g(
        &mut out,
        "fullnode_follow_step_micros_total",
        "Cumulative microseconds of whole follow steps (fetch wait + stage + VDF drain + confirm).",
        "counter",
        s.follow_step_micros,
    );
    g(
        &mut out,
        "fullnode_readahead_depth",
        "Current adaptive readahead depth K (windows fetched-or-in-flight ahead of the validator).",
        "gauge",
        s.readahead_depth,
    );
    g(
        &mut out,
        "fullnode_readahead_inflight_windows",
        "Windows currently fetched-or-in-flight in the follow readahead.",
        "gauge",
        s.readahead_inflight,
    );
    g(
        &mut out,
        "fullnode_readahead_hits_total",
        "Follow windows served from the readahead pipeline.",
        "counter",
        s.readahead_hits,
    );
    g(
        &mut out,
        "fullnode_readahead_misses_total",
        "Follow windows that fell back to a direct fetch (failed head fetch or replan).",
        "counter",
        s.readahead_misses,
    );
    g(
        &mut out,
        "fullnode_queue_resident_bytes",
        "Present block bytes held in the reorder buffer — the prefetch share of live allocation.",
        "gauge",
        s.queue_resident_bytes,
    );
    g(
        &mut out,
        "fullnode_outbound_tip",
        "Highest fresh OUTBOUND peer claim (the servable fetch frontier the follow producer clamps to); 0 = none.",
        "gauge",
        s.outbound_tip,
    );
    g(
        &mut out,
        "fullnode_queue_len",
        "Slots (in-flight + present) the reorder buffer holds ahead of the consumer.",
        "gauge",
        s.queue_len,
    );
    g(
        &mut out,
        "fullnode_window_blocks",
        "Blocks in the last sync window.",
        "gauge",
        s.window_blocks,
    );
    g(
        &mut out,
        "fullnode_window_tx_blocks",
        "Blocks in the last sync window carrying a transactions generator (the only ones window.body runs).",
        "gauge",
        s.window_tx_blocks,
    );
    g(
        &mut out,
        "fullnode_alloc_allocated_bytes",
        "jemalloc live allocated bytes (true retention signal).",
        "gauge",
        s.alloc_allocated,
    );
    g(
        &mut out,
        "fullnode_alloc_active_bytes",
        "jemalloc active bytes (pages backing live allocations; active - allocated = internal fragmentation).",
        "gauge",
        s.alloc_active,
    );
    g(
        &mut out,
        "fullnode_alloc_resident_bytes",
        "jemalloc resident bytes (allocated + allocator holdback).",
        "gauge",
        s.alloc_resident,
    );
    g(
        &mut out,
        "fullnode_alloc_retained_bytes",
        "jemalloc retained bytes (VM kept back from the OS, not in RSS; the allocator-retention half of the RSS attribution).",
        "gauge",
        s.alloc_retained,
    );
    g(
        &mut out,
        "fullnode_engine_cache_records",
        "Block records in the engine's bounded walk cache.",
        "gauge",
        s.engine_cache_records,
    );
    g(
        &mut out,
        "fullnode_engine_pending_orphans",
        "Blocks parked in the engine's pending-orphan map (parent unknown).",
        "gauge",
        s.engine_pending_orphans,
    );
    g(
        &mut out,
        "fullnode_engine_staged_generators",
        "Generators staged for the in-flight window (drained at confirm).",
        "gauge",
        s.engine_staged_generators,
    );
    g(
        &mut out,
        "fullnode_difficulty_window_cache_hits_total",
        "Consensus-walk records served from the in-memory record window (epoch-trough fix).",
        "counter",
        s.difficulty_window_cache_hits,
    );
    g(
        &mut out,
        "fullnode_difficulty_window_store_reads_total",
        "Consensus-walk records point-read from the store (cold start + per-peak head delta only once warm).",
        "counter",
        s.difficulty_window_store_reads,
    );
    g(
        &mut out,
        "fullnode_sync_from_height",
        "Configured --sync-from anchor height (0 = genesis).",
        "gauge",
        s.sync_from,
    );
    g(
        &mut out,
        "fullnode_net_bytes_in_total",
        "Bytes received on peer links (message payloads).",
        "counter",
        s.bytes_in,
    );
    g(
        &mut out,
        "fullnode_net_bytes_out_total",
        "Bytes sent on peer links (message payloads).",
        "counter",
        s.bytes_out,
    );
    g(
        &mut out,
        "fullnode_mempool_size",
        "Spend bundles resident in the mempool.",
        "gauge",
        s.mempool_size,
    );
    g(
        &mut out,
        "fullnode_mempool_cost",
        "Total CLVM cost resident in the mempool.",
        "gauge",
        s.mempool_cost,
    );
    g(
        &mut out,
        "fullnode_mempool_max_total_cost",
        "Mempool capacity ceiling in CLVM cost.",
        "gauge",
        s.mempool_max_cost,
    );
    g(
        &mut out,
        "fullnode_current_signage_point",
        "Index of the latest accepted signage point (0-63).",
        "gauge",
        s.sp_current_index,
    );
    g(
        &mut out,
        "fullnode_signage_points_total",
        "Signage points accepted since startup.",
        "counter",
        s.signage_points_total,
    );
    g(
        &mut out,
        "fullnode_last_reorg_depth",
        "Depth of the most recent reorg (0 = none observed).",
        "gauge",
        s.last_reorg_depth,
    );
    // Store commit/WAL/checkpoint telemetry — rendered only when the backend records it
    // (sqlite), so a backend without it exports NO store series rather than misleading zeros.
    if let Some(st) = &s.store {
        g(
            &mut out,
            "fullnode_sqlite_wal_bytes",
            "Current size of the SQLite -wal file in bytes (the bounded-WAL witness).",
            "gauge",
            st.wal_bytes,
        );
        g(
            &mut out,
            "fullnode_store_near_tip",
            "1 = near-tip band (per-block commits, active WAL checkpointer); 0 = catch-up band (window batch commits, checkpointer quiet).",
            "gauge",
            st.near_tip,
        );
        render_histogram(
            &mut out,
            "fullnode_store_commit_seconds",
            "Writer batch COMMIT latency (body-append and confirm transactions), by confirm phase.",
            &[
                ("catch_up", &st.commit_catch_up),
                ("near_tip", &st.commit_near_tip),
            ],
        );
        render_histogram(
            &mut out,
            "fullnode_sqlite_checkpoint_seconds",
            "PASSIVE WAL checkpoint duration on the dedicated checkpointer connection.",
            &[("", &st.checkpoint)],
        );
        g(
            &mut out,
            "fullnode_sqlite_wal_frames",
            "WAL length in frames as of the last checkpoint (the pragma's log column).",
            "gauge",
            st.wal_frames,
        );
        g(
            &mut out,
            "fullnode_sqlite_wal_frames_checkpointed_total",
            "WAL frames copied into the main DB by checkpoints.",
            "counter",
            st.wal_frames_checkpointed_total,
        );
        g(
            &mut out,
            "fullnode_sqlite_checkpoint_busy_total",
            "Checkpoints that returned busy (incomplete pass; persistent busy = checkpointer not keeping up).",
            "counter",
            st.checkpoint_busy_total,
        );
        g(
            &mut out,
            "fullnode_sqlite_checkpoint_errors_total",
            "Checkpoint pragmas that failed outright (WAL not being drained).",
            "counter",
            st.checkpoint_errors_total,
        );
        g(
            &mut out,
            "fullnode_store_record_reads_total",
            "Block-record point reads on the read path (each element of a multi-get counts) — rate against the window cadence attributes the staging loop's read serialization.",
            "counter",
            st.record_reads,
        );
        g(
            &mut out,
            "fullnode_store_coin_reads_total",
            "Coin-record point reads on the read path (each element of a multi-get counts) — the confirmed-set validation read volume.",
            "counter",
            st.coin_reads,
        );
        g(
            &mut out,
            "fullnode_sqlite_read_pool_idle",
            "Read-pool connections idle (in WAL mode an idle pooled reader can hold an old WAL read mark). High while wal_frames stays high names the reader pinning the checkpoint reset.",
            "gauge",
            st.read_pool_idle,
        );
        g(
            &mut out,
            "fullnode_sqlite_read_pool_size",
            "Read-pool total connections (idle + in-use).",
            "gauge",
            st.read_pool_size,
        );
    }
    // Block-producer pipeline — the first-block funnel. Read top-to-bottom: the first counter
    // that is 0 while the one above it is > 0 names the stalled stage, and the
    // candidates_dropped{reason} with the count is the exact wall.
    g(
        &mut out,
        "fullnode_producer_declares_received_total",
        "DeclareProofOfSpace messages received from farmers.",
        "counter",
        s.producer_declares_received,
    );
    g(
        &mut out,
        "fullnode_producer_candidates_built_total",
        "Candidate unfinished blocks assembled from accepted proofs.",
        "counter",
        s.producer_candidates_built,
    );
    g(
        &mut out,
        "fullnode_producer_request_signed_values_total",
        "RequestSignedValues messages returned to farmers to sign.",
        "counter",
        s.producer_request_signed_values,
    );
    g(
        &mut out,
        "fullnode_producer_signed_values_received_total",
        "SignedValues replies received from farmers.",
        "counter",
        s.producer_signed_values_received,
    );
    g(
        &mut out,
        "fullnode_producer_ub_assembled_total",
        "Finished unfinished blocks (farmer foliage signatures spliced).",
        "counter",
        s.producer_ub_assembled,
    );
    g(
        &mut out,
        "fullnode_producer_full_blocks_total",
        "Full blocks confirmed from OUR farmed unfinished blocks (terminal success).",
        "counter",
        s.producer_full_blocks,
    );
    out.push_str(
        "# HELP fullnode_producer_declares_validated_total Declares validated, by result.\n",
    );
    out.push_str("# TYPE fullnode_producer_declares_validated_total counter\n");
    for (result, n) in &s.producer_validated {
        out.push_str(&format!(
            "fullnode_producer_declares_validated_total{{result=\"{result}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_candidates_dropped_total Producer-pipeline drops, by reason.\n",
    );
    out.push_str("# TYPE fullnode_producer_candidates_dropped_total counter\n");
    for (reason, n) in &s.producer_candidates_dropped {
        out.push_str(&format!(
            "fullnode_producer_candidates_dropped_total{{reason=\"{reason}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_candidates_requeued_total Producer candidates re-queued after a transient wall (retried, not lost), by reason.\n",
    );
    out.push_str("# TYPE fullnode_producer_candidates_requeued_total counter\n");
    for (reason, n) in &s.producer_candidates_requeued {
        out.push_str(&format!(
            "fullnode_producer_candidates_requeued_total{{reason=\"{reason}\"}} {n}\n"
        ));
    }
    out.push_str(
        "# HELP fullnode_producer_ub_broadcast_total Unfinished-block announcements sent, by peer type.\n",
    );
    out.push_str("# TYPE fullnode_producer_ub_broadcast_total counter\n");
    for (pt, n) in &s.producer_ub_broadcast {
        out.push_str(&format!(
            "fullnode_producer_ub_broadcast_total{{peer_type=\"{pt}\"}} {n}\n"
        ));
    }
    // Per-message-type traffic — the gossip-health series (is each protocol conversation
    // actually flowing, in which direction, at what rate).
    out.push_str(
        "# HELP fullnode_net_messages_in_total Messages received on peer links, by type.\n",
    );
    out.push_str("# TYPE fullnode_net_messages_in_total counter\n");
    for (label, n) in &s.messages_in {
        out.push_str(&format!(
            "fullnode_net_messages_in_total{{msg=\"{label}\"}} {n}\n"
        ));
    }
    out.push_str("# HELP fullnode_net_messages_out_total Messages sent on peer links, by type.\n");
    out.push_str("# TYPE fullnode_net_messages_out_total counter\n");
    for (label, n) in &s.messages_out {
        out.push_str(&format!(
            "fullnode_net_messages_out_total{{msg=\"{label}\"}} {n}\n"
        ));
    }
    out
}

// Resident set size in bytes from /proc/self/statm (Linux): field 2 is resident pages. 0 on any platform or
// read that does not expose it (the deployed node is Linux; local dev may see 0).
// jemalloc's cached stats refresh on epoch advance; reads report the linked jemalloc's view
// (near-zero when a test binary runs on the system allocator instead).
fn jemalloc_stat_allocated() -> u64 {
    let _ = tikv_jemalloc_ctl::epoch::advance();
    tikv_jemalloc_ctl::stats::allocated::read().map_or(0, |v| v as u64)
}

fn jemalloc_stat_resident() -> u64 {
    tikv_jemalloc_ctl::stats::resident::read().map_or(0, |v| v as u64)
}

fn jemalloc_stat_active() -> u64 {
    tikv_jemalloc_ctl::stats::active::read().map_or(0, |v| v as u64)
}

fn jemalloc_stat_retained() -> u64 {
    tikv_jemalloc_ctl::stats::retained::read().map_or(0, |v| v as u64)
}

/// One structured startup memory self-report, logged by the daemon right after the engine walk cache
/// is warmed (the last big startup allocation). The mm-node OOM died 8 seconds after start with zero
/// allocation evidence — a pod that dies before its first Prometheus scrape still leaves this line
/// in the pod log: RSS, jemalloc's four views, and the walk-cache record count that drove them.
pub fn log_startup_memory(context: &'static str, walk_cache_records: usize) {
    info!(
        "startup memory self-report after engine walk-cache warm event={} context={} walk_cache_records={} rss_bytes={} alloc_allocated_bytes={} alloc_active_bytes={} alloc_resident_bytes={} alloc_retained_bytes={}",
        "fullnode.startup.memory",
        context,
        walk_cache_records,
        process_rss_bytes(),
        jemalloc_stat_allocated(),
        jemalloc_stat_active(),
        jemalloc_stat_resident(),
        jemalloc_stat_retained()
    );
}

fn process_rss_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let Some(resident_pages) = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse::<u64>().ok())
    else {
        return 0;
    };
    // Linux page size is 4 KiB on the deployment target; assume it rather than link libc for one syscall.
    resident_pages.saturating_mul(4096)
}

/// Bind and start the `/metrics` HTTP responder. Binds synchronously so a bind failure surfaces to the
/// caller; then spawns a bounded accept loop. Returns the run flag (clear it to stop the loop).
///
/// # Errors
/// Returns an I/O error if the address cannot be bound.
#[cfg(feature = "profiling")]
pub(crate) mod profiling {
    use super::*;

    // Sample the process for FLAMEGRAPH_SECONDS and stream back the flamegraph SVG. Runs on its own spawned task
    // (bounded to one concurrent via `profiling`) so /metrics keeps answering during the profile.

    // The whole profile — start guard, sample, symbolize, render — runs inside one `spawn_blocking` closure so the
    // `pprof::ProfilerGuard` is created and dropped on a single blocking thread (it never crosses an `.await`, so
    // the spawned future stays `Send`) and the CPU work never steals a runtime worker.
    pub(crate) async fn sample_flamegraph(seconds: u64) -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(move || {
            let guard = pprof::ProfilerGuardBuilder::default()
                .frequency(FLAMEGRAPH_HZ)
                .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                .build()
                .map_err(|e| format!("profiler start: {e}"))?;
            std::thread::sleep(Duration::from_secs(seconds));
            let report = guard
                .report()
                .build()
                .map_err(|e| format!("report build: {e}"))?;
            let mut svg = Vec::new();
            report
                .flamegraph(&mut svg)
                .map_err(|e| format!("flamegraph render: {e}"))?;
            Ok(svg)
        })
        .await
        .map_err(|e| format!("profiler task join: {e}"))?
    }

    // The decisive leak instrument: dump jemalloc's sampled heap profile (allocation-site stacks for the
    // LIVE bytes the process holds) and stream it back. Symbolize offline against the container binary:
    // `jeprof --show_bytes full-node heap.prof --pdf` or `pprof -http=: full-node heap.prof`.
    //
    // Requires jemalloc built with prof (`profiling` feature on tikv-jemallocator — the Linux target dep)
    // AND activated at PROCESS START via jemalloc's env var. tikv-jemalloc-sys builds with
    // --with-jemalloc-prefix=_rjem_, so the variable the linked jemalloc reads is `_RJEM_MALLOC_CONF`
    // (NOT plain `MALLOC_CONF`):
    //   _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19
    // `opt.prof` cannot be flipped at runtime — a deploy without the env var (or a prof-less build, e.g.
    // macOS dev) must fail LOUD here, never stream back an empty profile.

    // The mallctl work runs on the blocking pool: `prof.dump` writes the profile file synchronously
    // (file I/O inside jemalloc) and must not stall a runtime worker.
    pub(crate) async fn dump_heap_profile() -> Result<Vec<u8>, String> {
        tokio::task::spawn_blocking(jemalloc_prof_dump)
            .await
            .map_err(|e| format!("heap dump task join: {e}"))?
    }

    // The activation contract, checked loud-first (see `handle_heap`): `opt.prof` reads Err on a build
    // without jemalloc prof compiled in, `false` when compiled in but not enabled at start, and
    // `prof.active` is the runtime sampling switch. Only then is `prof.dump` worth issuing.
    const HEAP_PROF_HOWTO: &str =
        "start the process with _RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19";

    pub(super) fn jemalloc_prof_dump() -> Result<Vec<u8>, String> {
        // SAFETY (all three mallctl calls): `opt.prof` and `prof.active` are bool-typed mallctl keys read
        // as Rust `bool` (1 byte, matching jemalloc's C bool — the same shape tikv_jemalloc_ctl's own
        // `profiling::prof` wrapper uses); `prof.dump`'s new-value is a `*const c_char` pointing at a
        // NUL-terminated path that outlives the call (jemalloc uses it synchronously during the mallctl).
        let compiled =
            unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"opt.prof\0") }.map_err(|e| {
                format!(
                    "jemalloc heap profiling is not compiled into this build ({e}); \
                     build the Linux target with the tikv-jemallocator `profiling` feature and {HEAP_PROF_HOWTO}"
                )
            })?;
        if !compiled {
            return Err(format!(
                "jemalloc heap profiling is compiled in but was not enabled at process start; {HEAP_PROF_HOWTO}"
            ));
        }
        let active = unsafe { tikv_jemalloc_ctl::raw::read::<bool>(b"prof.active\0") }
            .map_err(|e| format!("failed to read prof.active ({e}); {HEAP_PROF_HOWTO}"))?;
        if !active {
            return Err(format!(
                "jemalloc heap profiling is enabled but sampling is inactive (prof_active:false); {HEAP_PROF_HOWTO}"
            ));
        }
        let path = std::env::temp_dir().join(format!(
            "full-node-heap-{}-{}.prof",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|e| format!("dump path contains NUL: {e}"))?;
        let dump = unsafe { tikv_jemalloc_ctl::raw::write(b"prof.dump\0", c_path.as_ptr()) };
        dump.map_err(|e| format!("prof.dump failed: {e}"))?;
        let prof = std::fs::read(&path).map_err(|e| format!("read dumped profile: {e}"));
        let _ = std::fs::remove_file(&path); // best-effort temp hygiene, success or not
        prof
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BOOT_GRACE_SECS, CONFIRM_MAX_SECS, HealthState, MetricsSnapshot, STALL_SECS, StoreSnapshot,
        render_metrics,
    };
    use dg_xch_stores::HistogramSnapshot;

    fn below_tip_with_peers() -> MetricsSnapshot {
        MetricsSnapshot {
            peak_height: 50,
            claimed_peak: 100,
            outbound_peers: 5,
            ..Default::default()
        }
    }

    const BOOT: u64 = 1_000_000;

    // Inside the boot grace window the probe is healthy even when below tip, with peers, and no
    // progress recorded — a cold start must not be killed while it fetches + verifies its first proof.
    #[test]
    fn boot_grace_is_healthy_even_when_below_tip() {
        let hs = HealthState::new_at(BOOT);
        let v = hs.verdict(&below_tip_with_peers(), BOOT + BOOT_GRACE_SECS - 1, 0);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("boot grace"), "body: {}", v.body);
    }

    // A node whose confirmed peak is at or above the best peer-announced tip is caught up: pausing
    // between blocks is not a stall, so it stays healthy however long since the last advance.
    #[test]
    fn caught_up_is_healthy() {
        let hs = HealthState::new_at(BOOT);
        let snap = MetricsSnapshot {
            peak_height: 100,
            claimed_peak: 100,
            outbound_peers: 5,
            ..Default::default()
        };
        // Far past grace and far past the stall window — caught-up must still win.
        let v = hs.verdict(&snap, BOOT + BOOT_GRACE_SECS + STALL_SECS * 10, 0);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("caught up"), "body: {}", v.body);
    }

    // No outbound peers = nothing to sync from; a restart cannot fix an empty address book, so the
    // probe must not thrash the pod for a network-side condition.
    #[test]
    fn no_peers_is_healthy() {
        let hs = HealthState::new_at(BOOT);
        let snap = MetricsSnapshot {
            peak_height: 50,
            claimed_peak: 100,
            outbound_peers: 0,
            ..Default::default()
        };
        let v = hs.verdict(&snap, BOOT + BOOT_GRACE_SECS + STALL_SECS + 5, 0);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("no outbound peers"), "body: {}", v.body);
    }

    // Below tip, with peers, past grace, but progress recorded inside the stall window: healthy.
    #[test]
    fn recent_progress_is_healthy() {
        let hs = HealthState::new_at(BOOT);
        let progressed_at = BOOT + BOOT_GRACE_SECS + 10;
        hs.observe(60, 0, progressed_at); // confirmed peak advanced 0 -> 60
        let v = hs.verdict(&below_tip_with_peers(), progressed_at + STALL_SECS - 1, 0);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("advancing"), "body: {}", v.body);
    }

    // The silent-stall signature: below tip, with peers, past grace, no advance for > STALL_SECS.
    // This is the exact condition the worker-thread panic produced and the one the orchestrator must restart.
    #[test]
    fn stalled_below_tip_is_unhealthy() {
        let hs = HealthState::new_at(BOOT);
        // last_progress is seeded to BOOT; evaluate well past both grace and the stall window.
        let now = BOOT + STALL_SECS + BOOT_GRACE_SECS + 1;
        let v = hs.verdict(&below_tip_with_peers(), now, 0);
        assert_eq!(v.status, "503 Service Unavailable");
        assert!(v.body.contains("stalled"), "body: {}", v.body);
    }

    // The download counter is a secondary progress witness: during fast-sync the confirmed peak does
    // not move until a window lands, but blocks_downloaded climbs — that must count as liveness.
    #[test]
    fn download_progress_alone_is_healthy() {
        let hs = HealthState::new_at(BOOT);
        let t = BOOT + BOOT_GRACE_SECS + 10;
        hs.observe(0, 5000, t); // peak flat, only blocks_downloaded advanced
        let v = hs.verdict(&below_tip_with_peers(), t + STALL_SECS - 1, 0);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("advancing"), "body: {}", v.body);
    }

    // A /debug/heap request against a process without ACTIVE jemalloc profiling
    // must fail loud with the exact activation env var — never resolve to an empty profile. This
    // holds in BOTH unprofiled environments: a prof-less build (macOS dev / non-`profiling`
    // feature: opt.prof itself is absent) and a prof-compiled build started without
    // `_RJEM_MALLOC_CONF` (opt.prof reads false). The only environment where it may succeed is a
    // live deploy that set the env var — which no test runner does.
    #[test]
    #[cfg(feature = "profiling")]
    fn heap_dump_without_active_profiling_fails_naming_the_env_var() {
        let err = super::profiling::jemalloc_prof_dump()
            .expect_err("dump must not succeed without _RJEM_MALLOC_CONF");
        assert!(
            err.contains("_RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19"),
            "error must carry the exact activation string, got: {err}"
        );
    }

    // The render must expose every counter/gauge as a Prometheus
    // series with its value, including the peak and claimed heights the dashboard graphs.
    #[test]
    fn render_exposes_all_series_with_values() {
        let mut commit_near_tip = HistogramSnapshot::default();
        commit_near_tip.buckets[3] = 7; // <= 0.1s
        commit_near_tip.count = 9;
        commit_near_tip.sum_micros = 1_500_000; // 1.5s total
        let snap = MetricsSnapshot {
            peak_height: 9_054_698,
            claimed_peak: 9_054_720,
            tip_lag: 22,
            sync_base_height: Some(4_575_000),
            store: Some(StoreSnapshot {
                wal_bytes: 487_000_000,
                near_tip: 1,
                commit_catch_up: HistogramSnapshot::default(),
                commit_near_tip,
                checkpoint: HistogramSnapshot {
                    buckets: [5; 12],
                    count: 6,
                    sum_micros: 250_000,
                },
                wal_frames: 1_200,
                wal_frames_checkpointed_total: 88_000,
                checkpoint_busy_total: 4,
                checkpoint_errors_total: 1,
                record_reads: 65_432,
                coin_reads: 12_345,
                read_pool_idle: 3,
                read_pool_size: 4,
            }),
            blocks_downloaded: 42,
            blocks_confirmed: 40,
            reclaimed: 3,
            peak_window: 256,
            peak_inflight_blocks: 32,
            rss_bytes: 123_456_789,
            outbound_peers: 8,
            inbound_peers: 5,
            inbound_connections: 37,
            window_vdf_micros: 2_660_000,
            window_sig_micros: 44_000,
            window_body_micros: 120_000,
            window_stage_micros: 1_900_000,
            window_confirm_micros: 9_500,
            window_blocks: 32,
            window_tx_blocks: 13,
            window_body_provided: 11,
            window_pre_wait_micros: 71_000,
            window_drain_wait_micros: 380_000,
            alloc_allocated: 900_000_000,
            alloc_active: 950_000_000,
            alloc_resident: 1_100_000_000,
            alloc_retained: 4_000_000_000,
            engine_cache_records: 4_096,
            engine_pending_orphans: 17,
            engine_staged_generators: 12,
            difficulty_window_cache_hits: 51_310,
            difficulty_window_store_reads: 32,
            follow_fetch_wait_micros: 777_000,
            follow_step_micros: 1_888_000,
            readahead_depth: 6,
            readahead_inflight: 4,
            readahead_hits: 30,
            readahead_misses: 2,
            queue_resident_bytes: 268_435_456,
            queue_len: 96,
            outbound_tip: 9_100_000,
            sync_from: 4_575_000,
            messages_in: vec![("new_peak", 7), ("new_transaction", 3)],
            messages_out: vec![("request_transaction", 2)],
            bytes_in: 10_240,
            bytes_out: 2_048,
            mempool_size: 5,
            mempool_cost: 1_000_000,
            mempool_max_cost: 550_000_000_000,
            sp_current_index: 41,
            signage_points_total: 1_234,
            last_reorg_depth: 2,
            producer_declares_received: 88,
            producer_candidates_built: 55,
            producer_request_signed_values: 44,
            producer_signed_values_received: 33,
            producer_ub_assembled: 22,
            producer_full_blocks: 11,
            producer_validated: vec![("accepted", 50), ("pospace_verify_fail", 4)],
            producer_candidates_dropped: vec![("no_timelord_peer", 6), ("ub_prev_unknown", 2)],
            producer_candidates_requeued: vec![("ub_prev_store_error", 3)],
            producer_ub_broadcast: vec![("timelord", 9)],
        };
        let text = render_metrics(&snap);
        assert!(text.contains("fullnode_peak_height 9054698"));
        assert!(text.contains("fullnode_claimed_peak_height 9054720"));
        assert!(text.contains("fullnode_blocks_downloaded_total 42"));
        assert!(text.contains("fullnode_blocks_confirmed_total 40"));
        assert!(text.contains("fullnode_reservations_reclaimed_total 3"));
        assert!(text.contains("fullnode_peak_reservation_window 256"));
        assert!(text.contains("fullnode_peak_inflight_blocks 32"));
        assert!(text.contains("fullnode_process_resident_bytes 123456789"));
        assert!(text.contains("fullnode_outbound_peers 8"));
        assert!(text.contains("fullnode_window_vdf_micros 2660000"));
        assert!(text.contains("fullnode_window_sig_micros 44000"));
        assert!(text.contains("fullnode_window_body_micros 120000"));
        assert!(text.contains("fullnode_window_stage_micros 1900000"));
        assert!(text.contains("fullnode_window_confirm_micros 9500"));
        assert!(text.contains("fullnode_window_blocks 32"));
        assert!(text.contains("fullnode_window_tx_blocks 13"));
        assert!(text.contains("fullnode_window_body_provided 11"));
        assert!(text.contains("fullnode_window_pre_wait_micros 71000"));
        assert!(text.contains("fullnode_window_drain_wait_micros 380000"));
        assert!(text.contains("fullnode_alloc_allocated_bytes 900000000"));
        assert!(text.contains("fullnode_alloc_resident_bytes 1100000000"));
        assert!(text.contains("fullnode_engine_cache_records 4096"));
        assert!(text.contains("fullnode_engine_pending_orphans 17"));
        assert!(text.contains("fullnode_engine_staged_generators 12"));
        assert!(text.contains("fullnode_difficulty_window_cache_hits_total 51310"));
        assert!(text.contains("fullnode_difficulty_window_store_reads_total 32"));
        assert!(text.contains("fullnode_follow_fetch_wait_micros_total 777000"));
        assert!(text.contains("fullnode_follow_step_micros_total 1888000"));
        assert!(text.contains("fullnode_readahead_depth 6"));
        assert!(text.contains("fullnode_queue_resident_bytes 268435456"));
        assert!(text.contains("fullnode_queue_len 96"));
        assert!(text.contains("fullnode_readahead_inflight_windows 4"));
        assert!(text.contains("fullnode_readahead_hits_total 30"));
        assert!(text.contains("fullnode_readahead_misses_total 2"));
        assert!(text.contains("fullnode_sync_from_height 4575000"));
        assert!(text.contains("fullnode_inbound_peers 5"));
        assert!(text.contains("fullnode_inbound_connections 37"));
        assert!(text.contains("fullnode_net_bytes_in_total 10240"));
        assert!(text.contains("fullnode_net_bytes_out_total 2048"));
        assert!(text.contains("fullnode_mempool_size 5"));
        assert!(text.contains("fullnode_mempool_cost 1000000"));
        assert!(text.contains("fullnode_mempool_max_total_cost 550000000000"));
        assert!(text.contains("fullnode_current_signage_point 41"));
        assert!(text.contains("fullnode_signage_points_total 1234"));
        assert!(text.contains("fullnode_last_reorg_depth 2"));
        assert!(text.contains("fullnode_producer_declares_received_total 88"));
        assert!(text.contains("fullnode_producer_candidates_built_total 55"));
        assert!(text.contains("fullnode_producer_request_signed_values_total 44"));
        assert!(text.contains("fullnode_producer_signed_values_received_total 33"));
        assert!(text.contains("fullnode_producer_ub_assembled_total 22"));
        assert!(text.contains("fullnode_producer_full_blocks_total 11"));
        assert!(
            text.contains("fullnode_producer_declares_validated_total{result=\"accepted\"} 50")
        );
        assert!(text.contains(
            "fullnode_producer_candidates_requeued_total{reason=\"ub_prev_store_error\"} 3"
        ));
        assert!(
            text.contains(
                "fullnode_producer_candidates_dropped_total{reason=\"no_timelord_peer\"} 6"
            )
        );
        assert!(text.contains("fullnode_producer_ub_broadcast_total{peer_type=\"timelord\"} 9"));
        assert!(text.contains("fullnode_net_messages_in_total{msg=\"new_peak\"} 7"));
        assert!(text.contains("fullnode_net_messages_in_total{msg=\"new_transaction\"} 3"));
        assert!(text.contains("fullnode_net_messages_out_total{msg=\"request_transaction\"} 2"));
        // TYPE lines are present (Prometheus rejects a series without one).
        assert!(text.contains("# TYPE fullnode_peak_height gauge"));
        assert!(text.contains("# TYPE fullnode_blocks_downloaded_total counter"));
        // The tip-lag / sync-floor / memory-attribution gauges.
        assert!(text.contains("fullnode_tip_lag 22"));
        assert!(text.contains("fullnode_sync_base_height 4575000"));
        assert!(text.contains("fullnode_alloc_active_bytes 950000000"));
        assert!(text.contains("fullnode_alloc_retained_bytes 4000000000"));
        // WAL + phase gauges.
        assert!(text.contains("fullnode_sqlite_wal_bytes 487000000"));
        assert!(text.contains("fullnode_store_near_tip 1"));
        assert!(text.contains("fullnode_sqlite_wal_frames 1200"));
        assert!(text.contains("fullnode_sqlite_wal_frames_checkpointed_total 88000"));
        assert!(text.contains("fullnode_sqlite_checkpoint_busy_total 4"));
        assert!(text.contains("fullnode_sqlite_checkpoint_errors_total 1"));
        assert!(text.contains("fullnode_store_record_reads_total 65432"));
        assert!(text.contains("fullnode_store_coin_reads_total 12345"));
        // The phase-labelled commit histogram in full Prometheus histogram shape —
        // cumulative buckets, +Inf, _sum in seconds, _count.
        assert!(text.contains("# TYPE fullnode_store_commit_seconds histogram"));
        assert!(
            text.contains("fullnode_store_commit_seconds_bucket{phase=\"near_tip\",le=\"0.1\"} 7")
        );
        assert!(
            text.contains("fullnode_store_commit_seconds_bucket{phase=\"near_tip\",le=\"+Inf\"} 9")
        );
        assert!(text.contains("fullnode_store_commit_seconds_sum{phase=\"near_tip\"} 1.5"));
        assert!(text.contains("fullnode_store_commit_seconds_count{phase=\"near_tip\"} 9"));
        assert!(
            text.contains("fullnode_store_commit_seconds_bucket{phase=\"catch_up\",le=\"+Inf\"} 0")
        );
        // The unlabelled checkpoint histogram renders bare le buckets.
        assert!(text.contains("fullnode_sqlite_checkpoint_seconds_bucket{le=\"0.01\"} 5"));
        assert!(text.contains("fullnode_sqlite_checkpoint_seconds_bucket{le=\"+Inf\"} 6"));
        assert!(text.contains("fullnode_sqlite_checkpoint_seconds_count 6"));
    }

    // A backend that records no store telemetry (mmap, postgres) must export NO store series at all
    // — zeros would read as "commits observed, all instant" and poison the latency queries.
    #[test]
    fn render_skips_store_series_when_backend_has_none() {
        let snap = MetricsSnapshot {
            peak_height: 10,
            claimed_peak: 12,
            tip_lag: 2,
            store: None,
            sync_base_height: None,
            ..Default::default()
        };
        let text = render_metrics(&snap);
        assert!(
            text.contains("fullnode_tip_lag 2"),
            "tip lag is backend-independent"
        );
        assert!(!text.contains("fullnode_sqlite_wal_bytes"));
        assert!(!text.contains("fullnode_store_commit_seconds"));
        assert!(!text.contains("fullnode_store_near_tip"));
        // An empty store has NO sync floor — absent, never a fake genesis 0.
        assert!(!text.contains("fullnode_sync_base_height"));
    }

    #[test]
    fn confirm_in_flight_is_healthy_though_peak_is_frozen() {
        let hs = HealthState::new_at(BOOT);
        // Evaluate long past both grace and the stall window with no observed progress.
        let now = BOOT + BOOT_GRACE_SECS + STALL_SECS * 3;
        // A confirm went in flight recently and is still running (well under the ceiling).
        let inflight_since = now - 40;
        let v = hs.verdict(&below_tip_with_peers(), now, inflight_since);
        assert_eq!(v.status, "200 OK");
        assert!(v.body.contains("confirm in flight"), "body: {}", v.body);
    }

    // The anti-unkillable guard: a confirm that has been "in flight" past CONFIRM_MAX_SECS with no
    // peak advance is not a slow write, it is a wedged/deadlocked writer — the node must still 503 so
    // the orchestrator can restart it. Without this ceiling a permanent deadlock would keep the pod
    // alive forever behind a stuck confirm.
    #[test]
    fn confirm_in_flight_past_ceiling_is_unhealthy() {
        let hs = HealthState::new_at(BOOT);
        let now = BOOT + BOOT_GRACE_SECS + STALL_SECS * 3;
        // In flight longer than the ceiling permits — a genuine deadlock, not a legitimate commit.
        let inflight_since = now - (CONFIRM_MAX_SECS + 5);
        let v = hs.verdict(&below_tip_with_peers(), now, inflight_since);
        assert_eq!(v.status, "503 Service Unavailable");
        assert!(v.body.contains("stalled"), "body: {}", v.body);
    }

    #[test]
    fn stall_dump_debounces_per_episode() {
        let hs = HealthState::new_at(BOOT);
        assert!(hs.should_dump_stall(), "first 503 of the episode dumps");
        assert!(!hs.should_dump_stall(), "second 503 is suppressed");
        assert!(!hs.should_dump_stall(), "third 503 is suppressed");
        hs.clear_stall(); // a 200 ends the episode
        assert!(hs.should_dump_stall(), "next episode dumps again");
    }
}
