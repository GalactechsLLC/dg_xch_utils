use super::{
    PrefetchConfig, READAHEAD_ABS_MAX_DEPTH, READAHEAD_BYTE_BUDGET, READAHEAD_MAX_DEPTH,
    READAHEAD_MAX_PER_PEER, READAHEAD_MIN_DEPTH, depth_within_budget,
};
use std::time::Duration;

#[test]
fn instant_takes_never_shrink_the_depth() {
    let mut depth = 64;
    let mut streak = 0;
    for _ in 0..10_000 {
        let (d, s) = super::next_depth(depth, streak, Duration::ZERO, 64);
        depth = d;
        streak = s;
    }
    assert_eq!(depth, 64, "a healthy full pipeline must keep its depth");
}

#[test]
fn a_waiting_take_grows_depth_to_the_ceiling() {
    let (d, s) = super::next_depth(8, 5, Duration::from_millis(60), 64);
    assert_eq!((d, s), (9, 0));
    let (d, _) = super::next_depth(64, 0, Duration::from_millis(60), 64);
    assert_eq!(d, 64, "growth clamps at max_depth");
}

#[test]
fn deadband_wait_holds_depth() {
    let (d, s) = super::next_depth(8, 31, Duration::from_millis(10), 64);
    assert_eq!(d, 8);
    assert_eq!(s, 0, "a measurable-but-small wait resets the streak");
}

#[test]
fn budget_clamps_depth_but_never_below_the_floor() {
    // 4 windows at 10 MiB each fit a 64 MiB budget.
    assert_eq!(depth_within_budget(4, 10 << 20, 64 << 20), 4);
    // 8 windows at 10 MiB do not: 64/10 = 6.
    assert_eq!(depth_within_budget(8, 10 << 20, 64 << 20), 6);
    // Giant windows clamp to the floor, never zero.
    assert_eq!(depth_within_budget(8, 512 << 20, 64 << 20), 1);
    // No EWMA yet: requested depth stands.
    assert_eq!(depth_within_budget(5, 0, 64 << 20), 5);
}

#[test]
fn default_config_reproduces_the_shipped_bounds() {
    let d = PrefetchConfig::default();
    assert_eq!(d.byte_budget, READAHEAD_BYTE_BUDGET);
    assert_eq!(d.byte_budget, 256 * 1024 * 1024);
    assert_eq!(d.max_depth, READAHEAD_MAX_DEPTH);
    assert_eq!(d.max_inflight, READAHEAD_MAX_DEPTH);
    assert_eq!(d.per_peer, 1);
}

#[test]
fn aggressive_scales_budget_depth_and_concurrency() {
    // 8 GiB, no explicit in-flight cap, planning for the 8-peer W==P target.
    let a = PrefetchConfig::aggressive(8192, None, 8);
    assert_eq!(a.byte_budget, 8192 * 1024 * 1024);
    // Aggregate in-flight defaults to the anti-flood ceiling: peers × per-peer cap = 8 × 16.
    assert_eq!(a.max_inflight, 8 * READAHEAD_MAX_PER_PEER);
    assert!(
        a.max_inflight > READAHEAD_MAX_DEPTH,
        "concurrency exceeds the shipped 8"
    );
    assert_eq!(
        a.max_depth, a.max_inflight,
        "resident depth ceiling tracks the aggregate"
    );
    // Spread ACROSS peers, not flooded onto one: ceil(128 / 8) = 16 per peer.
    assert_eq!(a.per_peer, a.max_inflight.div_ceil(8));
    assert!(a.per_peer <= READAHEAD_MAX_PER_PEER);
}

#[test]
fn explicit_max_inflight_sets_the_fanout() {
    let a = PrefetchConfig::aggressive(16384, Some(24), 8);
    assert_eq!(a.max_inflight, 24);
    // 24 requests across 8 peers = 3 per peer — raised aggregate, never per-peer flooding.
    assert_eq!(a.per_peer, 3);
    // Even a request cap below the shipped 8 keeps the resident-depth ceiling at the default
    // floor (aggressive never downgrades the shipped overlap).
    let small = PrefetchConfig::aggressive(16384, Some(2), 8);
    assert_eq!(small.max_inflight, 2);
    assert_eq!(small.max_depth, READAHEAD_MAX_DEPTH);
}

#[test]
fn bounds_are_hard_regardless_of_the_knob() {
    let a = PrefetchConfig::aggressive(u64::MAX, Some(1_000_000), 8);
    assert!(a.max_inflight <= READAHEAD_ABS_MAX_DEPTH);
    assert!(a.max_inflight <= 8 * READAHEAD_MAX_PER_PEER);
    assert!(a.per_peer <= READAHEAD_MAX_PER_PEER);
    assert!(a.max_depth <= READAHEAD_ABS_MAX_DEPTH);
}

// The budget bound is in bytes, so at huge block sizes the admissible depth collapses and the
// resident bodies never exceed the budget (bar the one floor window).
#[test]
fn huge_budget_collapses_depth_at_dust_era_block_sizes() {
    // A dust-era window measured at ~455 MiB/block × 32 heights ≈ 14.2 GiB.
    let dust_window_bytes: u64 = 455 * 1024 * 1024 * 32;
    // An ENORMOUS 64 GiB budget admits only a handful of such windows — depth collapses far
    // below the 128-window ceiling, and the resident bodies stay within the budget.
    let big = PrefetchConfig::aggressive(64 * 1024, None, 8);
    let admissible = depth_within_budget(big.max_depth, dust_window_bytes, big.byte_budget)
        .min(big.max_depth)
        .min(big.max_inflight);
    assert!(
        admissible < big.max_depth,
        "huge budget still collapses depth: {admissible} vs ceiling {}",
        big.max_depth
    );
    // Every window but the irreducible floor one fits inside the byte budget — the OOM ceiling.
    assert!(
        (admissible as u64)
            .saturating_sub(1)
            .saturating_mul(dust_window_bytes)
            <= big.byte_budget,
        "resident bodies (bar one floor window) stay within the byte budget"
    );
    // A budget smaller than a SINGLE dust-era window collapses all the way to the one-window
    // floor — the node can never hold more than the block it is validating.
    let tight = PrefetchConfig::aggressive(4 * 1024, None, 8); // 4 GiB < one ~14 GiB window
    let floored = depth_within_budget(tight.max_depth, dust_window_bytes, tight.byte_budget)
        .min(tight.max_depth)
        .min(tight.max_inflight);
    assert_eq!(
        floored, READAHEAD_MIN_DEPTH,
        "sub-window budget floors at one window"
    );
}
