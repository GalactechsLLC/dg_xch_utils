use super::StallWatchdog;
use crate::sync::SyncMetrics;
use crate::sync::queue::BlockQueue;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const TO: Duration = Duration::from_secs(60);

#[test]
fn a_frozen_frontier_with_work_and_peers_fires_after_the_timeout() {
    let t0 = Instant::now();
    let mut wd = StallWatchdog::new(100, t0, TO);
    // Just under the timeout with a frozen frontier: no fire yet.
    assert!(!wd.poll(100, t0 + TO - Duration::from_millis(1), true, true, false));
    // At/after the timeout, still frozen, work remains, peers live, no confirm in flight → FIRE.
    assert!(
        wd.poll(100, t0 + TO, true, true, false),
        "bounded stall must be reclaimed"
    );
}

#[test]
fn forward_progress_resets_the_clock_and_never_fires() {
    let t0 = Instant::now();
    let mut wd = StallWatchdog::new(100, t0, TO);
    // The frontier advances every tick well within the timeout — a healthy sync never reclaims.
    for k in 1..=10u64 {
        let now = t0 + Duration::from_secs(k * 10);
        assert!(
            !wd.poll(100 + k as u32, now, true, true, false),
            "advancing low_water is progress, not a stall"
        );
    }
}

#[test]
fn a_confirm_in_flight_suppresses_the_reclaim() {
    let t0 = Instant::now();
    let mut wd = StallWatchdog::new(100, t0, TO);
    // Frozen far past the timeout, but a confirm is legitimately in flight → never reclaim (a
    // rebase would drop the live window the consumer is validating).
    assert!(!wd.poll(100, t0 + TO * 10, true, true, true));
}

#[test]
fn caught_up_or_no_peers_never_fires() {
    let t0 = Instant::now();
    let mut wd = StallWatchdog::new(100, t0, TO);
    // No work (caught up): a still frontier is correct, not a stall.
    assert!(!wd.poll(100, t0 + TO * 5, false, true, false));
    // No live peers: nothing to reclaim toward; don't burn a rebase.
    assert!(!wd.poll(100, t0 + TO * 5, true, false, false));
}

#[test]
fn a_persistent_wedge_is_reclaimed_repeatedly_not_once() {
    let t0 = Instant::now();
    let mut wd = StallWatchdog::new(100, t0, TO);
    assert!(wd.poll(100, t0 + TO, true, true, false), "first reclaim");
    // The clock reset on the fire; still wedged → it must fire AGAIN after another full timeout,
    // not give up after one shot.
    assert!(!wd.poll(
        100,
        t0 + TO + TO - Duration::from_millis(1),
        true,
        true,
        false
    ));
    assert!(
        wd.poll(100, t0 + TO + TO, true, true, false),
        "second reclaim after another timeout"
    );
}

// Rebase-on-wedge against a real BlockQueue. That the rebase also wakes a producer parked on
// wait_space is covered in node/tests/block_queue.rs.
#[test]
fn tick_rebases_a_wedged_queue_and_increments_reclaimed() {
    let metrics = Arc::new(SyncMetrics::default());
    let q = BlockQueue::new(100, 1 << 20, metrics.clone());
    let gen0 = q.current_gen();

    let mut wd = StallWatchdog::new(q.low_water(), Instant::now(), Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(3));
    // Frontier frozen at 100, claimed target 10_000 >> 100, peers live, no confirm in flight → FIRE.
    let fired = wd.tick(&q, &metrics, Instant::now(), 10_000, true, false);

    assert!(fired, "a bounded stall with work + peers must reclaim");
    assert_eq!(
        metrics.reclaimed.load(Ordering::Relaxed),
        1,
        "reservations_reclaimed increments"
    );
    assert_ne!(
        q.current_gen(),
        gen0,
        "rebase bumped the generation (producer replan signal)"
    );
    assert_eq!(
        q.low_water(),
        100,
        "rebase held the frontier at the confirmed peak"
    );

    // Caught up (target == low_water) must NOT keep reclaiming: no work → no fire.
    let mut wd2 = StallWatchdog::new(q.low_water(), Instant::now(), Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(3));
    assert!(!wd2.tick(&q, &metrics, Instant::now(), q.low_water(), true, false));
    assert_eq!(
        metrics.reclaimed.load(Ordering::Relaxed),
        1,
        "no extra reclaim when caught up"
    );
}
