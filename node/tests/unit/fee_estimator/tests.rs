use super::*;

#[test]
fn buckets_span_initial_to_infinite() {
    let b = init_buckets();
    assert!(b.len() > 1);
    assert_eq!(b[0], INITIAL_STEP);
    assert_eq!(*b.last().unwrap(), INFINITE_FEE_RATE);
    // strictly increasing
    assert!(b.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn bucket_index_left_of_exact() {
    let b = init_buckets();
    // below the first bucket clamps to 0
    assert_eq!(get_bucket_index(&b, 0.0), 0);
    // way above the top clamps to the last
    assert_eq!(get_bucket_index(&b, 2.0 * INFINITE_FEE_RATE), b.len() - 1);
}

#[test]
fn empty_tracker_is_floor_zero() {
    let est = FeeEstimator::new(1_000_000);
    for t in [0u64, 40, 300, 3600] {
        assert_eq!(est.estimate_fee_rate(t), 0.0, "empty → floor 0 at t={t}");
    }
}

// Sustained identical-fee-rate blocks converge on a positive estimate, and higher pressure
// yields a strictly higher estimate. Txs confirm the next block (wait = 1), so even the
// shortest target has confirmation data.
fn drive_steady(fee: u64, cost: u64) -> FeeEstimator {
    let mut est = FeeEstimator::new(1_000_000);
    let wait = 1u32;
    for height in 100u32..300 {
        let included = vec![(cost, fee, height - wait)];
        est.new_block(height, &included, cost);
    }
    est
}

#[test]
fn steady_pressure_converges_positive() {
    let est = drive_steady(10_000_000, 5_000_000); // fee_per_cost = 2.0
    let rate = est.estimate_fee_rate(0);
    assert!(
        rate > 0.0,
        "sustained pressure must produce a positive estimate, got {rate}"
    );
}

#[test]
fn higher_pressure_higher_estimate() {
    let low = drive_steady(10_000_000, 5_000_000).estimate_fee_rate(0); // fpc 2
    let high = drive_steady(100_000_000, 5_000_000).estimate_fee_rate(0); // fpc 20
    assert!(
        low > 0.0 && high > 0.0,
        "both estimates positive: low={low} high={high}"
    );
    assert!(
        high > low,
        "higher fee-per-cost → higher estimate: low={low} high={high}"
    );
}

#[test]
fn tracker_state_advances_on_block() {
    let mut est = FeeEstimator::new(1_000_000);
    assert_eq!(est.tracker().latest_seen_height(), 0);
    assert_eq!(est.tracker().first_recorded_height(), 0);
    est.new_block(150, &[(5_000_000, 10_000_000, 145)], 5_000_000);
    assert_eq!(est.tracker().latest_seen_height(), 150);
    assert_eq!(est.tracker().first_recorded_height(), 150);
    // a reorg / non-advancing height is ignored
    est.new_block(150, &[(5_000_000, 10_000_000, 145)], 5_000_000);
    assert_eq!(est.tracker().latest_seen_height(), 150);
}

#[test]
fn add_then_remove_is_balanced() {
    // add_tx then remove_tx of the same item must not panic and must leave the estimator
    // queryable (the mempool eviction path).
    let mut est = FeeEstimator::new(1_000_000);
    est.new_block(100, &[(5_000_000, 10_000_000, 95)], 5_000_000);
    est.add_mempool_item(5_000_000, 10_000_000, 100, 5_000_000);
    est.remove_mempool_item(5_000_000, 10_000_000, 100, 0);
    let _ = est.estimate_fee_rate(60);
}
