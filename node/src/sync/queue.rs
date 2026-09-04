//! Bounded, byte-budgeted block reorder buffer (`BlockQueue`) decoupling the fetch producer from
//! the confirm consumer. Producers `complete` blocks out of height order (each window is fetched
//! from a distinct peer); the consumer `drain_next`s them strictly in height order. The bound is
//! measured in bytes, not a block count, because block wire size varies ~1 KiB … 455 MiB across eras.

use crate::sync::SyncMetrics;
use crate::sync::prefetch::approx_block_bytes;
use dg_xch_core::blockchain::full_block::FullBlock;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::sync::Notify;

/// Block height — the queue's sequence space (the reorder key).
pub type Height = u32;

/// A reorder-buffer cell. A height is in exactly one of {absent, `InFlight`, `Present`, consumed},
/// so a scheduler never double-assigns and a hedge loser's late arrival is a
/// no-op on an already-`Present`/consumed slot.
enum Slot {
    /// Reserved by a peer that is fetching it; carries the hard reclaim deadline. `generation` is
    /// stamped at `admit` and required back at `complete` for the stale-branch guard.
    InFlight {
        #[allow(dead_code)]
        peer_id: u64,
        #[allow(dead_code)]
        deadline: Instant,
        #[allow(dead_code)]
        generation: u64,
    },
    /// Fetched and resident; `nbytes` is its wire-size charge against the byte budget.
    Present { block: Box<FullBlock>, nbytes: u64 },
}

struct Inner {
    /// Reorder buffer keyed by height. Out-of-order `complete` is `O(log n)`; the consumer only ever
    /// touches `first_key_value`.
    slots: BTreeMap<Height, Slot>,
    /// Resident PRESENT bytes (`B` in the invariants). `InFlight` slots carry no bytes — their size is
    /// unknown until they complete, at which point `B` may overshoot the budget by at most one block
    /// (the deliberate over-fill bias).
    bytes: u64,
    /// The consumer's next needed height (`= confirmed_peak + 1`); monotone non-decreasing under
    /// `drain_*`; the sole operation that lowers it is `rebase` on a reorg, in lockstep with the
    /// engine peak.
    low_water: Height,
    /// Reorg generation, bumped by every `rebase`. A completion whose generation no longer matches
    /// was fetched on a superseded branch and is dropped. Task-abort of a pre-rebase fetch is racy
    /// (a task past its last `.await` still runs to `complete`), so the queue is the authority on
    /// staleness.
    generation: u64,
}

/// A byte-bounded, height-keyed reorder buffer decoupling fetch (producer) from confirm (consumer).
///
/// A producer is admitted only while `B < budget` (checked pre-add, so overshoot is at most one
/// block), parking on [`BlockQueue::wait_space`] otherwise; the consumer parks on
/// [`BlockQueue::wait_ready`] only when its exact head height is missing. Producers wait on space
/// the consumer releases and the consumer waits on the head a producer completes, so the two waits
/// form no cycle.
pub struct BlockQueue {
    inner: Mutex<Inner>,
    /// Hard byte ceiling `W` (default [`crate::sync::READAHEAD_BYTE_BUDGET`] = 256 MiB).
    budget: u64,
    resident_bytes: AtomicU64,
    /// Producer wakeup: fired when a `drain_next` frees budget (all parked producers re-contend).
    space: Notify,
    /// Consumer wakeup: fired when the slot at `low_water` becomes `Present`.
    ready: Notify,
    /// Producer cancellation: fired whenever a rebase invalidates the current fetch plan.
    replan: Notify,
    metrics: Arc<SyncMetrics>,
}

impl BlockQueue {
    /// A queue whose consumer starts at `low_water` (= `confirmed_peak + 1`), bounded by `budget`
    /// resident bytes.
    #[must_use]
    pub fn new(low_water: Height, budget: u64, metrics: Arc<SyncMetrics>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                slots: BTreeMap::new(),
                bytes: 0,
                low_water,
                generation: 0,
            }),
            budget,
            resident_bytes: AtomicU64::new(0),
            space: Notify::new(),
            ready: Notify::new(),
            replan: Notify::new(),
            metrics,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resident PRESENT bytes `B` (the gauge value).
    #[must_use]
    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes.load(Ordering::Relaxed)
    }

    /// The hard byte ceiling `W`.
    #[must_use]
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Slots currently held (`InFlight` + `Present`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().slots.len()
    }

    /// `true` when the buffer holds no slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().slots.is_empty()
    }

    /// The consumer's next needed height.
    #[must_use]
    pub fn low_water(&self) -> Height {
        self.lock().low_water
    }

    /// The current reorg generation — a producer reads this when it dispatches a fetch and carries it
    /// back into [`BlockQueue::complete`], so a completion that lands after a [`BlockQueue::rebase`]
    /// (which bumped the generation) is recognised as stale and dropped.
    #[must_use]
    pub fn current_gen(&self) -> u64 {
        self.lock().generation
    }

    #[must_use]
    pub fn can_admit(&self) -> bool {
        self.resident_bytes.load(Ordering::Relaxed) < self.budget
    }

    /// Reserve `height` for `peer_id` with a hard reclaim `deadline` (the scheduler's claim). No
    /// effect if the height is already tracked — a producer never clobbers a `Present` or a live claim.
    pub fn admit(&self, height: Height, peer_id: u64, deadline: Instant) {
        let mut inner = self.lock();
        if height < inner.low_water || inner.slots.contains_key(&height) {
            return;
        }
        let generation = inner.generation;
        inner.slots.insert(
            height,
            Slot::InFlight {
                peer_id,
                deadline,
                generation,
            },
        );
    }

    /// Mark `height` fetched and resident: `InFlight → Present` (or insert `Present` directly),
    /// charge its wire bytes, and signal `ready` if it is the head. A block below `low_water`, a
    /// duplicate `Present`, or a completion whose `generation` no longer matches the queue is
    /// dropped. The producer passes the generation it read via [`BlockQueue::current_gen`] when it
    /// dispatched the fetch.
    pub fn complete(&self, block: FullBlock, generation: u64) {
        let height = block.height();
        let nbytes = approx_block_bytes(&block);
        let mut inner = self.lock();
        if generation != inner.generation {
            return; // stale generation — fetched before a rebase, drop (ABA guard)
        }
        if height < inner.low_water {
            return; // already consumed — hedge/late duplicate, drop
        }
        if matches!(inner.slots.get(&height), Some(Slot::Present { .. })) {
            return; // duplicate present (hedge loser) — drop
        }
        inner.slots.insert(
            height,
            Slot::Present {
                block: Box::new(block),
                nbytes,
            },
        );
        inner.bytes = inner.bytes.saturating_add(nbytes);
        let is_head = height == inner.low_water;
        self.resident_bytes.store(inner.bytes, Ordering::Relaxed);
        self.publish_gauges(&inner);
        drop(inner);
        if is_head {
            self.ready.notify_waiters();
        }
    }

    /// Pop the head block iff it is `Present` at `low_water`; advance `low_water`, release its bytes,
    /// and wake parked producers. Returns `None` immediately when the head is absent or still
    /// `InFlight` — the consumer then `wait_ready`s, never blocking on a deeper slow window.
    pub fn drain_next(&self) -> Option<FullBlock> {
        let mut inner = self.lock();
        let head = inner.low_water;
        if !matches!(inner.slots.get(&head), Some(Slot::Present { .. })) {
            return None; // head absent or still InFlight — never surface a non-head slot
        }
        let Some(Slot::Present { block, nbytes }) = inner.slots.remove(&head) else {
            return None;
        };
        inner.bytes = inner.bytes.saturating_sub(nbytes);
        inner.low_water = inner.low_water.saturating_add(1);
        self.resident_bytes.store(inner.bytes, Ordering::Relaxed);
        self.publish_gauges(&inner);
        drop(inner);
        self.space.notify_waiters();
        Some(*block)
    }

    /// Drain the maximal contiguous run of `Present` blocks starting at `low_water`, up to `max`
    /// blocks, in one lock acquisition — the consumer's batch pull for the validator's height-ordered
    /// window. Returns an empty vec when the head is absent or still `InFlight`. Advances
    /// `low_water` past every drained block and releases their bytes, waking producers.
    pub fn drain_ready_window(&self, max: u32) -> Vec<FullBlock> {
        let mut inner = self.lock();
        let mut out = Vec::new();
        while (out.len() as u32) < max {
            let head = inner.low_water;
            let Some(Slot::Present { .. }) = inner.slots.get(&head) else {
                break; // head absent or InFlight — never surface a non-head slot
            };
            let Some(Slot::Present { block, nbytes }) = inner.slots.remove(&head) else {
                break;
            };
            inner.bytes = inner.bytes.saturating_sub(nbytes);
            inner.low_water = inner.low_water.saturating_add(1);
            out.push(*block);
        }
        if !out.is_empty() {
            self.resident_bytes.store(inner.bytes, Ordering::Relaxed);
            self.publish_gauges(&inner);
            drop(inner);
            self.space.notify_waiters();
        }
        out
    }

    /// Clone the contiguous ready run at the head WITHOUT draining it — the body-precompute
    /// pipeline's view of the NEXT window while the current one validates. Returns the same
    /// blocks a subsequent [`BlockQueue::drain_ready_window`] would surface (provided no rebase
    /// or reclaim intervenes), never advances `low_water`, and never uncharges bytes.
    #[must_use]
    pub fn peek_ready_window(&self, max: u32) -> Vec<FullBlock> {
        let inner = self.lock();
        let mut out = Vec::new();
        let mut head = inner.low_water;
        while (out.len() as u32) < max {
            let Some(Slot::Present { block, .. }) = inner.slots.get(&head) else {
                break;
            };
            out.push((**block).clone());
            head = head.saturating_add(1);
        }
        out
    }

    /// Reset the consumer's head to `new_low_water` and invalidate every outstanding fetch. The
    /// driver calls this in lockstep with an engine peak change (reorg or forward jump) to restore
    /// `low_water == confirmed_peak + 1`. Every queued slot is dropped, the byte charge is zeroed,
    /// the generation is bumped so late in-flight completions are dropped by the
    /// [`BlockQueue::complete`] guard, and parked producers are woken to replan. Deliberately does
    /// NOT signal `ready`: the new head is absent and must be re-fetched.
    pub fn rebase(&self, new_low_water: Height) {
        let mut inner = self.lock();
        inner.slots.clear();
        inner.bytes = 0;
        inner.low_water = new_low_water;
        inner.generation = inner.generation.wrapping_add(1);
        self.resident_bytes.store(0, Ordering::Relaxed);
        self.publish_gauges(&inner);
        drop(inner);
        self.space.notify_waiters();
        self.replan.notify_waiters();
    }

    /// Release an `InFlight` claim (the scheduler's stall reclaim): the height returns to
    /// absent so another peer can take it. No effect on a `Present` slot.
    pub fn reclaim(&self, height: Height) {
        let mut inner = self.lock();
        if matches!(inner.slots.get(&height), Some(Slot::InFlight { .. })) {
            inner.slots.remove(&height);
        }
    }

    /// The heights in `[low_water, low_water + window)` that are neither `Present` nor `InFlight` — the
    /// scheduler's fill targets (`FindNextBlocksToDownload` shape), lowest first.
    #[must_use]
    pub fn gaps(&self, window: u32) -> Vec<Height> {
        let inner = self.lock();
        let start = inner.low_water;
        let end = start.saturating_add(window);
        (start..end)
            .filter(|h| !inner.slots.contains_key(h))
            .collect()
    }

    /// The next height a producer should fetch: one past the highest slot currently held, or
    /// `low_water` when the buffer is empty. A `rebase` resets it (via `low_water`) so the producer
    /// re-plans on the new branch without a separate cursor.
    #[must_use]
    pub fn next_fetch_height(&self) -> Height {
        let inner = self.lock();
        inner
            .slots
            .keys()
            .next_back()
            .map_or(inner.low_water, |h| h.saturating_add(1))
    }

    /// `true` when the head height is `Present` (the consumer can drain without parking).
    #[must_use]
    pub fn head_ready(&self) -> bool {
        let inner = self.lock();
        matches!(
            inner.slots.get(&inner.low_water),
            Some(Slot::Present { .. })
        )
    }

    /// Park until the head height is `Present`, then return. The consumer is blocked only on its
    /// own next block, never on a deeper window.
    ///
    /// The `notified()` future is created before the condition check: a `Notified` captures a
    /// `notify_waiters` from creation time, so a wakeup between check and await cannot be lost.
    pub async fn wait_ready(&self) {
        loop {
            let notified = self.ready.notified();
            if self.head_ready() {
                return;
            }
            notified.await;
        }
    }

    /// Park until resident PRESENT bytes fall below the budget. Same create-before-check
    /// discipline as [`BlockQueue::wait_ready`].
    pub async fn wait_space(&self) {
        loop {
            let notified = self.space.notified();
            if self.can_admit() {
                return;
            }
            notified.await;
        }
    }

    /// Wait until `generation` is no longer current. Fetchers select this beside network waits so a
    /// watchdog/reorg rebase cancels the stale operation immediately rather than waiting for it to
    /// return before observing the generation bump.
    pub async fn wait_replan(&self, generation: u64) {
        loop {
            let notified = self.replan.notified();
            if self.current_gen() != generation {
                return;
            }
            notified.await;
        }
    }

    fn publish_gauges(&self, inner: &Inner) {
        self.metrics
            .queue_resident_bytes
            .store(inner.bytes, Ordering::Relaxed);
        self.metrics
            .queue_len
            .store(inner.slots.len() as u64, Ordering::Relaxed);
        // peak_window charts heights held ahead of the consumer.
        self.metrics
            .peak_window
            .fetch_max(inner.slots.len(), Ordering::Relaxed);
    }
}
