mod common;

use common::{load_full_block, restamp_block};
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_node::sync::SyncMetrics;
use dg_xch_node::sync::queue::{BlockQueue, Height};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn base() -> FullBlock {
    // The lighter of the two fixtures — the queue only reads height + wire size.
    load_full_block(5_000_004)
}

fn block_at(base: &FullBlock, h: Height) -> FullBlock {
    restamp_block(base, h)
}

fn queue(low_water: Height, budget: u64) -> BlockQueue {
    BlockQueue::new(low_water, budget, Arc::new(SyncMetrics::default()))
}

// Pass-through complete at the queue's CURRENT generation — the producer's normal path (no rebase in
// flight). The generation-guard cases below pass an explicit stale gen instead.
fn put(q: &BlockQueue, blk: FullBlock) {
    q.complete(blk, q.current_gen());
}

// Deterministic xorshift — a property sweep without a rand/proptest dependency (the offline builder
// pins Cargo.lock; a new dev-dep is a heavier change than a 5-line PRNG).
struct XorShift(u64);
impl XorShift {
    fn next_u32(&mut self, bound: u32) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x % u64::from(bound)) as u32
    }
}

#[test]
fn drains_in_height_order_regardless_of_complete_order() {
    let b = base();
    let q = queue(100, 1 << 30);
    for h in [103u32, 100, 102, 101, 104] {
        put(&q, block_at(&b, h));
    }
    for want in 100..=104 {
        let got = q.drain_next().expect("head present");
        assert_eq!(got.height(), want, "consumer must see strict height order");
    }
    assert!(q.drain_next().is_none(), "drained dry");
}

#[test]
fn a_gap_blocks_only_the_head_not_deeper_present_slots() {
    let b = base();
    let q = queue(0, 1 << 30);
    put(&q, block_at(&b, 1));
    put(&q, block_at(&b, 2));
    assert!(!q.head_ready(), "head 0 absent → not ready");
    assert!(q.drain_next().is_none(), "must not surface a non-head slot");
    put(&q, block_at(&b, 0));
    assert!(q.head_ready());
    for want in 0..=2 {
        assert_eq!(q.drain_next().unwrap().height(), want);
    }
}

#[test]
fn blocks_below_low_water_and_duplicates_are_dropped() {
    let b = base();
    let q = queue(50, 1 << 30);
    put(&q, block_at(&b, 49)); // already consumed
    assert!(q.is_empty(), "sub-low-water block dropped");
    put(&q, block_at(&b, 50));
    put(&q, block_at(&b, 50)); // duplicate (hedge loser)
    assert_eq!(q.len(), 1, "duplicate present dropped");
}

#[test]
fn low_water_is_monotone_and_bytes_balance_to_zero() {
    let b = base();
    let q = queue(0, 1 << 30);
    for h in 0..10 {
        put(&q, block_at(&b, h));
    }
    assert!(q.resident_bytes() > 0);
    for want in 0..10 {
        assert_eq!(q.low_water(), want);
        q.drain_next().unwrap();
    }
    assert_eq!(q.low_water(), 10, "low_water advanced monotonically");
    assert_eq!(
        q.resident_bytes(),
        0,
        "every admitted byte released on drain"
    );
}

#[test]
fn admit_reclaim_and_gaps_track_the_in_flight_set() {
    let b = base();
    let q = queue(0, 1 << 30);
    let dl = Instant::now() + Duration::from_secs(30);
    q.admit(0, 1, dl);
    q.admit(2, 2, dl);
    assert_eq!(q.gaps(4), vec![1, 3], "gaps are the uncovered heights");
    assert!(q.drain_next().is_none(), "InFlight head is not drainable");
    assert_eq!(q.resident_bytes(), 0, "InFlight carries no bytes");
    q.reclaim(0);
    assert_eq!(q.gaps(4), vec![0, 1, 3], "reclaim reopens the height");
    put(&q, block_at(&b, 2)); // InFlight -> Present
    assert!(q.resident_bytes() > 0);
}

#[test]
fn can_admit_reflects_the_byte_budget_over_fill_bias() {
    let b = base();
    let one = queue(0, 1 << 30);
    put(&one, block_at(&b, 0));
    let one_block = one.resident_bytes();
    assert!(one_block > 0);
    let q = queue(0, one_block / 2); // budget below a single block
    assert!(
        q.can_admit(),
        "empty queue admits even an over-budget block (over-fill bias)"
    );
    put(&q, block_at(&b, 0));
    assert!(
        !q.can_admit(),
        "past budget, admission refused until a drain"
    );
    q.drain_next().unwrap();
    assert!(q.can_admit(), "drain frees space");
}

// Property: for many random out-of-order completion schedules of [0, N), the consumer always sees the
// exact ascending sequence and the byte accounting returns to zero.
#[test]
fn property_random_completion_orders_reproduce_the_ascending_sequence() {
    let b = base();
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for trial in 0..80u32 {
        let n: u32 = 1 + rng.next_u32(24); // 1..=24 heights
        let q = queue(0, 1 << 30);
        let mut order: Vec<u32> = (0..n).collect();
        for i in (1..n as usize).rev() {
            let j = rng.next_u32((i + 1) as u32) as usize;
            order.swap(i, j);
        }
        for &h in &order {
            put(&q, block_at(&b, h));
        }
        for want in 0..n {
            let got = q
                .drain_next()
                .unwrap_or_else(|| panic!("trial {trial}: missing head {want}"));
            assert_eq!(got.height(), want, "trial {trial}: out-of-order drain");
        }
        assert!(q.drain_next().is_none(), "trial {trial}: over-drain");
        assert_eq!(q.resident_bytes(), 0, "trial {trial}: byte leak");
        assert_eq!(q.low_water(), n, "trial {trial}: low_water end");
    }
}

// Backpressure liveness: a producer parked on a full budget is released the instant the consumer
// drains — the bounded-buffer full/empty handshake on the tokio runtime.
#[tokio::test]
async fn wait_space_wakes_on_drain() {
    let b = base();
    let one = queue(0, 1 << 30);
    put(&one, block_at(&b, 0));
    let one_block = one.resident_bytes();
    let q = Arc::new(queue(0, one_block)); // room for exactly one block
    put(&q, block_at(&b, 0));
    assert!(!q.can_admit(), "at budget");
    let q2 = q.clone();
    let waiter = tokio::spawn(async move { q2.wait_space().await });
    tokio::task::yield_now().await;
    assert_eq!(q.drain_next().unwrap().height(), 0);
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("producer released within the deadline")
        .expect("waiter task ok");
}

// The consumer parks on an absent head and is woken the instant the head completes.
#[tokio::test]
async fn wait_ready_wakes_on_head_complete() {
    let b = base();
    let q = Arc::new(queue(0, 1 << 30));
    let q2 = q.clone();
    let waiter = tokio::spawn(async move {
        q2.wait_ready().await;
        q2.drain_next().map(|blk| blk.height())
    });
    tokio::task::yield_now().await;
    put(&q, block_at(&b, 0));
    let got = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("consumer released within the deadline")
        .expect("waiter task ok");
    assert_eq!(got, Some(0));
}

#[tokio::test]
async fn wait_replan_wakes_on_generation_change() {
    let q = Arc::new(queue(100, 1 << 30));
    let generation = q.current_gen();
    let q2 = q.clone();
    let waiter = tokio::spawn(async move { q2.wait_replan(generation).await });
    tokio::task::yield_now().await;

    q.rebase(100);
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("rebase interrupts a stale fetch promptly")
        .expect("waiter task ok");
}

// drain_ready_window pulls the maximal contiguous PRESENT run from low_water in one call (the
// consumer's batch pull that keeps the frozen core's window-precompute intact), stops at the first
// gap, and is capped by `max`.
#[test]
fn drain_ready_window_pulls_the_contiguous_run_capped_by_max() {
    let b = base();
    let q = queue(0, 1 << 30);
    for h in [0u32, 1, 2, 3, /* gap at 4 */ 5, 6] {
        put(&q, block_at(&b, h));
    }
    let w = q.drain_ready_window(32);
    assert_eq!(
        w.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![0, 1, 2, 3],
        "stops at the gap, never surfacing 5/6"
    );
    assert_eq!(q.low_water(), 4, "low_water at the gap");
    assert!(q.drain_ready_window(32).is_empty(), "head 4 absent → empty");
    put(&q, block_at(&b, 4));
    let capped = q.drain_ready_window(2);
    assert_eq!(
        capped.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![4, 5],
        "max caps the batch at 2"
    );
    assert_eq!(q.low_water(), 6);
}

// The generation guard: a completion carrying the pre-rebase generation is a provable
// no-op after a rebase bumped it — the deterministic ABA/stale-branch drop.
#[test]
fn generation_guard_drops_a_stale_gen_completion_after_rebase() {
    let b = base();
    let q = queue(100, 1 << 30);
    let stale_gen = q.current_gen();
    // A producer dispatched a fetch for height 100 under `stale_gen`; before it lands, a reorg rebases.
    q.rebase(100);
    assert_ne!(q.current_gen(), stale_gen, "rebase bumped the generation");
    // The late old-branch completion arrives — dropped by the guard.
    q.complete(block_at(&b, 100), stale_gen);
    assert!(q.is_empty(), "stale-gen completion dropped");
    assert!(
        !q.head_ready(),
        "head still absent — must be re-fetched on the new branch"
    );
    // A fresh completion at the CURRENT generation is accepted normally.
    q.complete(block_at(&b, 100), q.current_gen());
    assert!(q.head_ready(), "fresh-gen completion admitted");
}

// rebase clears every stale slot at/above the new head, zeroes the byte charge exactly, resets
// low_water (the reorg-backward case), and wakes producers without signalling the (now-absent) head.
#[test]
fn rebase_clears_stale_slots_and_resets_low_water() {
    let b = base();
    let q = queue(100, 1 << 30);
    // Confirmed peak was 104 (low_water 105); the producer queued 105..=108 on the old branch.
    // Re-stamp low_water forward first by draining a run, then queue ahead.
    for h in 100..=108 {
        put(&q, block_at(&b, h));
    }
    for _ in 100..105 {
        q.drain_next().unwrap(); // consumer confirmed 100..=104
    }
    assert_eq!(q.low_water(), 105);
    assert!(
        q.resident_bytes() > 0,
        "105..=108 resident on the old branch"
    );
    // Reorg discovered: fork at height 102 → new low_water = 103. Every queued slot ≥103 is stale.
    q.rebase(103);
    assert_eq!(q.low_water(), 103, "head rewound to fork+1");
    assert_eq!(q.resident_bytes(), 0, "old-branch byte charge fully freed");
    assert!(q.is_empty(), "every stale slot dropped");
    assert!(
        !q.head_ready(),
        "new head absent → consumer parks, no false ready"
    );
}

// rebase forward (a driver-side bulk/anchor/infusion peak jump) also fully reclaims the below-head
// byte charge that a split_off form would leak.
#[test]
fn rebase_forward_reclaims_all_bytes() {
    let b = base();
    let q = queue(0, 1 << 30);
    for h in 0..5 {
        put(&q, block_at(&b, h));
    }
    assert!(q.resident_bytes() > 0);
    // bulk_sync jumped the engine peak to 999; the queued 0..5 are all below the new head.
    q.rebase(1000);
    assert_eq!(q.low_water(), 1000);
    assert_eq!(q.resident_bytes(), 0, "no byte leak on a forward rebase");
    assert!(q.is_empty());
}

// THE phase-2 decoupling property: the consumer drains every already-present block to completion while a
// producer task is BLOCKED on a full byte budget (parked in `wait_space`). Consumer progress is
// independent of producer progress — the whole point of the bounded buffer. The producer only unblocks
// once the consumer's drains have freed budget, and it never gated the consumer's forward progress.
#[tokio::test]
async fn consumer_drains_to_completion_while_the_producer_is_blocked() {
    let b = base();
    let sizer = queue(0, 1 << 30);
    put(&sizer, block_at(&b, 0));
    let one_block = sizer.resident_bytes();
    // Budget for exactly three resident blocks; pre-fill heights 0,1,2 so the queue is at budget.
    let q = Arc::new(queue(0, one_block * 3));
    for h in 0..3 {
        put(&q, block_at(&b, h));
    }
    assert!(!q.can_admit(), "queue at budget → producer must block");

    // Producer: wants to admit height 3 but is parked until the consumer frees budget. It records the
    // instant it finally gets in, so we can prove it ran strictly AFTER consumer progress.
    let qp = q.clone();
    let producer = tokio::spawn(async move {
        qp.wait_space().await; // blocked here until a drain frees a slot
        put(&qp, block_at(&base(), 3));
    });
    tokio::task::yield_now().await;

    // Consumer: drains the three present blocks in order while the producer is still parked.
    let drained = q.drain_ready_window(32);
    assert_eq!(
        drained.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "consumer made full forward progress with the producer blocked"
    );
    assert_eq!(q.low_water(), 3);

    // Freed budget now releases the producer; height 3 lands and the consumer drains it too.
    tokio::time::timeout(Duration::from_secs(5), producer)
        .await
        .expect("producer released after the consumer freed budget")
        .expect("producer task ok");
    let tail = q.drain_ready_window(32);
    assert_eq!(
        tail.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![3]
    );
    assert_eq!(q.resident_bytes(), 0, "byte accounting balanced to zero");
}

// Phase-3 producer contract (what the detached fetch_scheduler relies on): a fill loop that completes
// blocks while `can_admit` fills the queue to the byte budget — biased to over-fill by exactly the one
// block that crosses the line — and then STOPS. This is the backpressure that keeps the producer from
// unbounded fetch-ahead: it fills to budget and no further until a consumer drain frees space.
#[test]
fn producer_fills_to_budget_then_stops() {
    let b = base();
    let sizer = queue(0, 1 << 30);
    put(&sizer, block_at(&b, 0));
    let one_block = sizer.resident_bytes();
    assert!(one_block > 0);
    let budget = one_block * 4 + one_block / 2; // room for four full blocks + a half
    let q = queue(0, budget);
    let mut admitted = 0u32;
    while q.can_admit() {
        put(&q, block_at(&b, admitted));
        admitted += 1;
        assert!(
            admitted < 100,
            "fill must terminate at the budget, never run away"
        );
    }
    assert_eq!(
        admitted, 5,
        "four blocks under budget + one over-fill block, then stop"
    );
    assert!(
        q.resident_bytes() >= budget,
        "the producer fills to at least the budget (over-fill bias, never under)"
    );
    assert!(
        q.resident_bytes() <= budget + one_block,
        "overshoot bounded to a single block"
    );
    // A drain frees budget and the producer can admit again — backpressure releases on consumption.
    q.drain_next().unwrap();
    assert!(q.can_admit(), "a consumer drain re-opens admission");
}

// ── sync-decoupling liveness backstop (deadlock fix) ─────────────────────────────────────────────

// The stall watchdog's recovery action is `rebase(low_water)`. Beyond zeroing the byte charge and
// bumping the generation (the producer's replan signal), it MUST wake a producer parked on wait_space
// — otherwise a reclaimed pipeline can't actually resume. Proven here against the real queue: a
// producer parked at the budget is released the instant the watchdog rebases.
#[tokio::test]
async fn rebase_wakes_a_parked_producer() {
    let b = base();
    let one = queue(0, 1 << 30);
    put(&one, block_at(&b, 0));
    let one_block = one.resident_bytes();
    let q = Arc::new(queue(0, one_block)); // room for exactly one block
    put(&q, block_at(&b, 0));
    assert!(!q.can_admit(), "at budget → producer parks in wait_space");
    let q2 = q.clone();
    let waiter = tokio::spawn(async move { q2.wait_space().await });
    tokio::task::yield_now().await;
    // The watchdog fires: force-rebase to the confirmed frontier (no forward move — a stall reclaim).
    q.rebase(q.low_water());
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the watchdog's rebase released the parked producer within the deadline")
        .expect("waiter task ok");
    assert!(
        q.can_admit(),
        "rebase cleared the byte charge → the producer can admit again"
    );
}

// Why wait_space/wait_ready create the `Notified` BEFORE the condition check (the lost-wakeup
// discipline). A tokio `Notified` snapshots the `notify_waiters` generation at CREATION — NOT at first
// poll — so a `notify_waiters` that lands after the future is created but before it is first polled is
// STILL delivered. That is exactly the drain→notify that races the producer's `can_admit` check: as
// long as `notified()` is created before the check, the wakeup cannot slip through. (This test also
// documents the empirical tokio-1.53 semantics that make the create-before-check pattern safe without
// needing `enable()`.)
#[tokio::test]
async fn notified_created_before_the_check_captures_a_later_notify_waiters() {
    use std::future::Future;
    use std::task::{Context, Poll};
    use tokio::sync::Notify;

    let notify = Notify::new();
    let mut cx = Context::from_waker(std::task::Waker::noop());

    // Create the Notified (as wait_space does, before its check); THEN a notify_waiters fires; THEN the
    // first poll. It must resolve Ready — the wakeup is captured, not lost.
    let n = notify.notified();
    tokio::pin!(n);
    notify.notify_waiters();
    assert!(
        matches!(n.as_mut().poll(&mut cx), Poll::Ready(())),
        "a Notified created before notify_waiters captures it on first poll (generation snapshot at \
         creation) — this is what makes the create-before-check wait pattern lost-wakeup free"
    );

    // Contrast: a Notified created AFTER the notify_waiters does not see that past notification — which
    // is why the pattern relies on creating the future before the condition check, not after it.
    let n2 = notify.notified();
    tokio::pin!(n2);
    assert!(
        matches!(n2.as_mut().poll(&mut cx), Poll::Pending),
        "a Notified created after the notify_waiters is not retroactively notified"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_wait_notify_makes_progress_under_load() {
    let b = base();
    let one = queue(0, 1 << 30);
    put(&one, block_at(&b, 0));
    let one_block = one.resident_bytes();
    let q = Arc::new(queue(0, one_block)); // exactly one block fits
    const ROUNDS: u32 = 20_000;

    let qp = q.clone();
    let producer = tokio::spawn(async move {
        let bb = base();
        for h in 0..ROUNDS {
            qp.wait_space().await; // park until the consumer drains the prior block
            qp.complete(block_at(&bb, h), qp.current_gen());
        }
    });
    let qc = q.clone();
    let consumer = tokio::spawn(async move {
        for _ in 0..ROUNDS {
            qc.wait_ready().await; // park until the producer completes the head
            assert!(
                qc.drain_next().is_some(),
                "head must be present after wait_ready"
            );
        }
    });

    tokio::time::timeout(Duration::from_secs(30), async {
        producer.await.expect("producer ok");
        consumer.await.expect("consumer ok");
    })
    .await
    .expect("wait/notify handoff must not lose a wakeup and wedge the pipeline");
    assert_eq!(q.low_water(), ROUNDS, "every round handed off");
}

// ---------------------------------------------------------------------------
// peek_ready_window — the body-precompute pipeline's lookahead view.
// ---------------------------------------------------------------------------

#[test]
fn peek_returns_the_ready_run_without_draining() {
    let b = base();
    let q = queue(10, u64::MAX);
    for h in [10u32, 11, 12] {
        put(&q, block_at(&b, h));
    }
    let peeked = q.peek_ready_window(8);
    assert_eq!(
        peeked.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![10, 11, 12],
        "peek surfaces the contiguous head run"
    );
    assert_eq!(q.low_water(), 10, "peek never advances the head");
    let drained = q.drain_ready_window(8);
    assert_eq!(
        drained.iter().map(FullBlock::height).collect::<Vec<_>>(),
        vec![10, 11, 12],
        "a subsequent drain surfaces exactly what peek showed"
    );
}

#[test]
fn peek_stops_at_a_gap_and_respects_max() {
    let b = base();
    let q = queue(0, u64::MAX);
    for h in [0u32, 1, 3] {
        put(&q, block_at(&b, h));
    }
    assert_eq!(
        q.peek_ready_window(8)
            .iter()
            .map(FullBlock::height)
            .collect::<Vec<_>>(),
        vec![0, 1],
        "the gap at 2 ends the run"
    );
    assert_eq!(q.peek_ready_window(1).len(), 1, "max caps the peek");
}

#[test]
fn peek_after_drain_shows_the_next_window() {
    let b = base();
    let q = queue(0, u64::MAX);
    for h in 0u32..6 {
        put(&q, block_at(&b, h));
    }
    let first = q.drain_ready_window(3);
    assert_eq!(first.len(), 3);
    assert_eq!(
        q.peek_ready_window(3)
            .iter()
            .map(FullBlock::height)
            .collect::<Vec<_>>(),
        vec![3, 4, 5],
        "after draining window N, peek shows window N+1"
    );
}
