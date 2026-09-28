//! Progress watchdog for the decoupled fetch/confirm pipeline.
//!
//! Individual fetches are timeout-bounded, but nothing else detects the pipeline as a whole ceasing
//! to make progress. If the queue's `low_water` stops advancing for `timeout` while there is work to
//! do, peers are live, and no confirm is in flight, force a [`BlockQueue::rebase`] to the current
//! `low_water`: the generation bump is the `fetch_scheduler`'s abort-and-replan signal, and any
//! producer parked on `wait_space` is woken. The reclaim is counted in `SyncMetrics::reclaimed`.
//!
//! Keyed purely on `Height`/`Instant`/booleans so the decision is unit-testable with injected clocks.

use crate::sync::SyncMetrics;
use crate::sync::queue::{BlockQueue, Height};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Tracks the confirmed-peak frontier over time and fires a bounded stall reclaim when it wedges.
pub struct StallWatchdog {
    /// Highest `low_water` observed so far; a strictly-greater value is real forward progress.
    last_low_water: Height,
    /// When `last_low_water` last advanced (or when a not-actionable state last reset the clock).
    last_advance: Instant,
    /// How long the frontier may sit still — while actionable — before a reclaim is forced.
    timeout: Duration,
}

impl StallWatchdog {
    /// Start tracking from `low_water` at `now`, firing after `timeout` of actionable no-progress.
    #[must_use]
    pub fn new(low_water: Height, now: Instant, timeout: Duration) -> Self {
        Self {
            last_low_water: low_water,
            last_advance: now,
            timeout,
        }
    }

    /// Returns `true` exactly when the frontier has not advanced for `>= timeout` AND the stall is
    /// actionable: work remains, peers are live, no confirm in flight. Every other case resets the
    /// clock. A fire also resets the clock, so a persistent wedge is reclaimed repeatedly.
    pub fn poll(
        &mut self,
        low_water: Height,
        now: Instant,
        has_work: bool,
        peers_live: bool,
        confirm_in_flight: bool,
    ) -> bool {
        if low_water > self.last_low_water {
            self.last_low_water = low_water;
            self.last_advance = now;
            return false;
        }
        if !has_work || !peers_live || confirm_in_flight {
            // Not a stall we should act on — don't accumulate time toward a reclaim.
            self.last_advance = now;
            return false;
        }
        if now.duration_since(self.last_advance) >= self.timeout {
            self.last_advance = now;
            return true;
        }
        false
    }

    /// One driver-tick evaluation against the live queue and metrics. When [`StallWatchdog::poll`]
    /// fires, force `queue.rebase(low_water)` and count the reclaim in `metrics.reclaimed`.
    /// Returns whether a reclaim was performed. `sync_target` is the heaviest claimed height.
    pub fn tick(
        &mut self,
        queue: &BlockQueue,
        metrics: &SyncMetrics,
        now: Instant,
        sync_target: Height,
        peers_live: bool,
        confirm_in_flight: bool,
    ) -> bool {
        let low_water = queue.low_water();
        let has_work = sync_target > low_water;
        if self.poll(low_water, now, has_work, peers_live, confirm_in_flight) {
            queue.rebase(low_water);
            metrics.reclaimed.fetch_add(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/sync/watchdog/tests.rs"]
mod tests;
