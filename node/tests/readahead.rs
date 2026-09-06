// Readahead bookkeeping — behavioral pins for the K-deep multi-peer window pipeline. The
// properties that must hold before the follow driver may trust it:
//   1. adjacent pending windows dispatch to DISTINCT peers, contiguously ahead of the validator;
//   2. a slow peer serving window N+3 never stalls the take (hence the confirm) of window N;
//   3. a failed head fetch falls back (None) but KEEPS the deeper windows in flight;
//   4. a plan mismatch aborts everything — the next fill replans from the caller's truth;
//   5. abort/drop cancels every in-flight task (no leaks, nothing ever written anywhere);
//   6. the depth adapts grow-only: measured take-wait grows K; instant takes hold it;
//   7. zero state: a fresh readahead reports no signal (class-7 truthfulness).

mod common;

use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_node::SyncMetrics;
use dg_xch_node::sync::source::BlockRangeSource;
use dg_xch_node::sync::{
    PrefetchConfig, READAHEAD_MAX_DEPTH, READAHEAD_START_DEPTH, SyncError, WindowReadahead,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(30);
const BATCH: u32 = 32;

// A scripted peer: serves re-stamped bodies after `delay` (or hangs forever), records every
// range it was asked for, and counts live fetch futures — the leak witness (a dropped/aborted
// fetch must decrement on Drop).
struct ScriptedSource {
    id: u64,
    base: FullBlock,
    delay: Duration,
    hang: bool,
    fail: bool,
    live: Arc<AtomicUsize>,
    served: Arc<Mutex<Vec<(u64, u32, u32)>>>,
}

struct LiveGuard(Arc<AtomicUsize>);
impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl BlockRangeSource for ScriptedSource {
    fn peer_id(&self) -> u64 {
        self.id
    }
    fn is_closed(&self) -> bool {
        false
    }
    async fn fetch_range(&self, start: u32, end: u32) -> Result<Vec<FullBlock>, SyncError> {
        self.live.fetch_add(1, Ordering::SeqCst);
        let _g = LiveGuard(self.live.clone());
        self.served
            .lock()
            .expect("served")
            .push((self.id, start, end));
        if self.fail {
            return Err(SyncError::RangeRejected { start, end });
        }
        if self.hang {
            std::future::pending::<()>().await;
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok((start..=end)
            .map(|h| common::restamp_block(&self.base, h))
            .collect())
    }
}

struct Rig {
    live: Arc<AtomicUsize>,
    served: Arc<Mutex<Vec<(u64, u32, u32)>>>,
    base: FullBlock,
}

impl Rig {
    fn new() -> Self {
        Self {
            live: Arc::new(AtomicUsize::new(0)),
            served: Arc::new(Mutex::new(Vec::new())),
            base: common::load_full_block(5_000_000),
        }
    }
    fn peer(&self, id: u64) -> Arc<dyn BlockRangeSource> {
        self.scripted(id, Duration::ZERO, false, false)
    }
    fn scripted(
        &self,
        id: u64,
        delay: Duration,
        hang: bool,
        fail: bool,
    ) -> Arc<dyn BlockRangeSource> {
        Arc::new(ScriptedSource {
            id,
            base: self.base.clone(),
            delay,
            hang,
            fail,
            live: self.live.clone(),
            served: self.served.clone(),
        })
    }
    async fn drained(&self) -> bool {
        for _ in 0..200 {
            if self.live.load(Ordering::SeqCst) == 0 {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

fn readahead() -> WindowReadahead {
    WindowReadahead::new(Arc::new(SyncMetrics::default()), TIMEOUT)
}

// Property 1 — striping: START_DEPTH adjacent contiguous windows, each on a DIFFERENT peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adjacent_windows_dispatch_to_distinct_peers() {
    let rig = Rig::new();
    let sources: Vec<_> = (0..8).map(|i| rig.peer(i)).collect();
    let mut ra = readahead();
    ra.fill(&sources, 100, 10_000, BATCH);
    assert_eq!(ra.inflight(), READAHEAD_START_DEPTH);
    // Wait for every dispatched task to have recorded its range.
    for _ in 0..200 {
        if rig.served.lock().expect("served").len() >= READAHEAD_START_DEPTH {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut served = rig.served.lock().expect("served").clone();
    served.sort_by_key(|&(_, s, _)| s);
    let expected: Vec<(u32, u32)> = (0..READAHEAD_START_DEPTH as u32)
        .map(|i| (100 + i * BATCH, 100 + i * BATCH + BATCH - 1))
        .collect();
    assert_eq!(
        served.iter().map(|&(_, s, e)| (s, e)).collect::<Vec<_>>(),
        expected,
        "windows are adjacent and contiguous"
    );
    let mut peers: Vec<u64> = served.iter().map(|&(p, _, _)| p).collect();
    peers.sort_unstable();
    peers.dedup();
    assert_eq!(
        peers.len(),
        READAHEAD_START_DEPTH,
        "each adjacent window went to a different peer: {served:?}"
    );
    ra.abort_all();
    assert!(rig.drained().await, "aborted fetches must all drop");
}

// Property 2 — THE stall pin: a peer serving window N+3 that never answers must not delay the
// take of windows N, N+1, N+2 (each served by its own live peer).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_peer_deep_in_the_pipeline_never_stalls_the_head() {
    let rig = Rig::new();
    // Rotation starts at source 0: windows N..N+3 land on peers 0,1,2,3 in order; peer 3 hangs.
    let sources: Vec<_> = (0..4)
        .map(|i| rig.scripted(i, Duration::ZERO, i == 3, false))
        .collect();
    let mut ra = readahead();
    ra.fill(&sources, 0, 10_000, BATCH);
    assert_eq!(ra.inflight(), 4);
    for w in 0..3u32 {
        let (from, to) = (w * BATCH, w * BATCH + BATCH - 1);
        let taken = tokio::time::timeout(Duration::from_secs(5), ra.take(from, to))
            .await
            .unwrap_or_else(|_| panic!("take of window {w} stalled behind the hung peer"));
        let blocks = taken.expect("window served");
        assert_eq!(blocks.len(), BATCH as usize);
        assert_eq!(blocks[0].height(), from);
        assert_eq!(blocks.last().expect("nonempty").height(), to);
    }
    // The hung window is still (only) what's in flight; killing the readahead reaps it.
    assert_eq!(ra.inflight(), 1);
    drop(ra);
    assert!(
        rig.drained().await,
        "dropping the readahead reaps the hung fetch"
    );
}

// Property 3 — failed head falls back, tail survives: the caller direct-fetches the failed
// window while the deeper windows stay in flight and serve the following takes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_head_fetch_keeps_the_deeper_windows() {
    let rig = Rig::new();
    let sources: Vec<_> = (0..4)
        .map(|i| rig.scripted(i, Duration::ZERO, false, i == 0))
        .collect();
    let mut ra = readahead();
    ra.fill(&sources, 0, 10_000, BATCH);
    assert_eq!(ra.inflight(), 4);
    assert!(
        ra.take(0, BATCH - 1).await.is_none(),
        "head window's peer rejected: take reports the miss"
    );
    assert_eq!(
        ra.inflight(),
        3,
        "deeper windows must survive the head failure"
    );
    let next = ra
        .take(BATCH, 2 * BATCH - 1)
        .await
        .expect("next window still served from the pipeline");
    assert_eq!(next[0].height(), BATCH);
    assert_eq!(ra.metrics().readahead_hits.load(Ordering::Relaxed), 1);
    assert_eq!(ra.metrics().readahead_misses.load(Ordering::Relaxed), 1);
    ra.abort_all();
    assert!(rig.drained().await);
}

// Property 4 — plan mismatch aborts everything: a request that is not the pipeline head means
// the base or the claimed cap moved; stale plans must never be served.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mismatched_request_aborts_the_plan() {
    let rig = Rig::new();
    let sources: Vec<_> = (0..4).map(|i| rig.peer(i)).collect();
    let mut ra = readahead();
    ra.fill(&sources, 100, 10_000, BATCH);
    assert_eq!(ra.inflight(), READAHEAD_START_DEPTH);
    assert!(
        ra.take(500, 500 + BATCH - 1).await.is_none(),
        "a base jump is a miss"
    );
    assert_eq!(ra.inflight(), 0, "every stale window must be aborted");
    assert!(rig.drained().await, "aborted fetches must all drop");
    // Windows entirely below the new base are dropped even when deeper ones would match.
    ra.fill(&sources, 100, 10_000, BATCH);
    let second = ra
        .take(100 + BATCH, 100 + 2 * BATCH - 1)
        .await
        .expect("the second window matches after the stale head is dropped");
    assert_eq!(second[0].height(), 100 + BATCH);
    ra.abort_all();
    assert!(rig.drained().await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn depth_grows_on_wait_and_holds_on_instant_takes() {
    let rig = Rig::new();
    let slow: Vec<_> = (0..8)
        .map(|i| rig.scripted(i, Duration::from_millis(120), false, false))
        .collect();
    let mut ra = readahead();
    assert_eq!(ra.depth(), READAHEAD_START_DEPTH);
    ra.fill(&slow, 0, 100_000, BATCH);
    assert!(
        ra.take(0, BATCH - 1).await.is_some(),
        "slow window still serves"
    );
    assert_eq!(
        ra.depth(),
        READAHEAD_START_DEPTH + 1,
        "a measured wait grows the depth"
    );
    assert!(ra.depth() <= READAHEAD_MAX_DEPTH);
    let fast: Vec<_> = (0..8).map(|i| rig.peer(8 + i)).collect();
    let mut from = BATCH;
    for _ in 0..64 {
        ra.fill(&fast, from, 1_000_000, BATCH);
        tokio::time::sleep(Duration::from_millis(15)).await;
        if ra.take(from, from + BATCH - 1).await.is_some() {
            from += BATCH;
        }
        assert!(
            ra.depth() > READAHEAD_START_DEPTH,
            "instant takes must never shrink the depth"
        );
    }
    ra.abort_all();
    assert!(rig.drained().await);
}

// Property 7 — zero state (class-7 truthfulness): a fresh readahead reports no signal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_readahead_reports_zero_signals() {
    let ra = readahead();
    let m = ra.metrics();
    assert_eq!(ra.inflight(), 0);
    assert_eq!(m.readahead_hits.load(Ordering::Relaxed), 0);
    assert_eq!(m.readahead_misses.load(Ordering::Relaxed), 0);
    assert_eq!(m.follow_fetch_wait_micros.load(Ordering::Relaxed), 0);
    assert_eq!(m.follow_step_micros.load(Ordering::Relaxed), 0);
    assert_eq!(m.readahead_inflight.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropping_readahead_clears_the_inflight_gauge() {
    let rig = Rig::new();
    let metrics = Arc::new(SyncMetrics::default());
    let mut ra = WindowReadahead::new(metrics.clone(), TIMEOUT);
    let sources: Vec<_> = (0..4)
        .map(|i| rig.scripted(i, Duration::ZERO, true, false))
        .collect();
    ra.fill(&sources, 0, 10_000, BATCH);
    assert_eq!(
        metrics.readahead_inflight.load(Ordering::Relaxed),
        READAHEAD_START_DEPTH as u64
    );

    drop(ra);
    assert_eq!(metrics.readahead_inflight.load(Ordering::Relaxed), 0);
    assert!(rig.drained().await, "drop aborts every fetch future");
}

fn served_by_peer(rig: &Rig) -> HashMap<u64, usize> {
    let mut per_peer: HashMap<u64, usize> = HashMap::new();
    for (p, _, _) in rig.served.lock().expect("served").iter() {
        *per_peer.entry(*p).or_default() += 1;
    }
    per_peer
}

async fn wait_for_served(rig: &Rig, n: usize) {
    for _ in 0..200 {
        if rig.served.lock().expect("served").len() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// Aggressive knob — unset is UNCHANGED: the default config keeps the one-window-per-peer cap, so
// with only 2 peers just 2 windows dispatch even though START_DEPTH is 4 (byte-identical to the shipped default —
// the `busy_peer` exclusion is exactly "already carries a window").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_config_keeps_one_window_per_peer() {
    let rig = Rig::new();
    let sources: Vec<_> = (0..2).map(|i| rig.peer(i)).collect();
    let mut ra = WindowReadahead::with_config(
        Arc::new(SyncMetrics::default()),
        TIMEOUT,
        PrefetchConfig::default(),
    );
    ra.fill(&sources, 0, 10_000, BATCH);
    assert_eq!(
        ra.inflight(),
        2,
        "default per-peer cap of 1 limits inflight to the peer count"
    );
    wait_for_served(&rig, 2).await;
    let per_peer = served_by_peer(&rig);
    assert!(
        per_peer.values().all(|&c| c == 1),
        "no peer carries more than one window by default: {per_peer:?}"
    );
    ra.abort_all();
    assert!(rig.drained().await);
}

// Aggressive knob — per-peer fan-out: with the operator knob set, the readahead may place MORE THAN
// ONE in-flight window on a peer (the lifted `busy_peer` cap), spreading the raised concurrency
// ACROSS the available peers rather than flooding one. Two peers with START_DEPTH=4 admissible → each
// carries two, bounded by the per-peer cap — deterministic on the first fill (no ramp needed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aggressive_knob_stacks_bounded_windows_per_peer() {
    let rig = Rig::new();
    let sources: Vec<_> = (0..2).map(|i| rig.peer(i)).collect();
    // Huge byte budget (no clamp here), aggregate in-flight cap 8, 2 planning peers → per_peer = 4.
    let cfg = PrefetchConfig::aggressive(1 << 20, Some(8), 2);
    assert!(
        cfg.per_peer >= 2,
        "aggressive fan-out lifts the one-per-peer cap"
    );
    let mut ra = WindowReadahead::with_config(Arc::new(SyncMetrics::default()), TIMEOUT, cfg);
    ra.fill(&sources, 0, 10_000, BATCH);
    // Depth still STARTS at START_DEPTH (the OOM-safe ramp), now spread over only 2 peers.
    assert_eq!(ra.inflight(), READAHEAD_START_DEPTH);
    wait_for_served(&rig, READAHEAD_START_DEPTH).await;
    let per_peer = served_by_peer(&rig);
    assert_eq!(
        per_peer.len(),
        2,
        "both peers used, spread not flooded: {per_peer:?}"
    );
    assert!(
        per_peer.values().all(|&c| c <= cfg.per_peer) && per_peer.values().any(|&c| c >= 2),
        "fan-out stacks >1 window on a peer but stays under the per-peer cap: {per_peer:?}"
    );
    ra.abort_all();
    assert!(rig.drained().await);
}

// Aggressive knob — lifted depth ceiling drives concurrency: under sustained validator wait the
// adaptive depth K climbs PAST the shipped MAX_DEPTH (8) when the knob raises the ceiling, and the
// aggregate in-flight windows exceed the peer count — the concurrency that keeps a fast validator fed
// and drives follow_fetch_wait_micros down as the buffer deepens.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn aggressive_depth_climbs_past_the_shipped_cap() {
    let rig = Rig::new();
    // Few peers + a per-fetch delay keeps the validator measurably waiting, so K ratchets up.
    let sources: Vec<_> = (0..2)
        .map(|i| rig.scripted(i, Duration::from_millis(80), false, false))
        .collect();
    let cfg = PrefetchConfig::aggressive(1 << 20, Some(24), 2); // per_peer 12, max_depth 24
    assert!(cfg.max_depth > READAHEAD_MAX_DEPTH);
    let mut ra = WindowReadahead::with_config(Arc::new(SyncMetrics::default()), TIMEOUT, cfg);
    let mut max_depth_seen = 0usize;
    let mut max_inflight_seen = 0usize;
    let mut from = 0u32;
    for _ in 0..300 {
        ra.fill(&sources, from, 1_000_000, BATCH);
        max_inflight_seen = max_inflight_seen.max(ra.inflight());
        if ra.take(from, from + BATCH - 1).await.is_some() {
            from += BATCH;
        }
        max_depth_seen = max_depth_seen.max(ra.depth());
        if max_depth_seen > READAHEAD_MAX_DEPTH && max_inflight_seen > 2 {
            break;
        }
    }
    assert!(
        max_depth_seen > READAHEAD_MAX_DEPTH,
        "adaptive depth must climb past the shipped {READAHEAD_MAX_DEPTH}-window cap: saw {max_depth_seen}"
    );
    assert!(
        max_inflight_seen > 2,
        "aggregate in-flight exceeds the peer count (concurrency past one-per-peer): saw {max_inflight_seen}"
    );
    // The wait gauge was exercised — the signal that must trend toward zero as K rises.
    assert!(
        ra.metrics()
            .follow_fetch_wait_micros
            .load(Ordering::Relaxed)
            > 0
    );
    ra.abort_all();
    assert!(rig.drained().await);
}
