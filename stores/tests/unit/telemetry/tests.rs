use super::{DURATION_BUCKETS_SECS, DurationHistogram};
use std::sync::atomic::Ordering;

// Cumulative bucket semantics: an observation lands in its bucket and every wider one, and the
// +Inf count includes observations past the last bound.
#[test]
fn record_is_cumulative() {
    let h = DurationHistogram::default();
    h.record(0.03); // > 0.025, <= 0.05
    h.record(0.03);
    h.record(500.0); // past the last bound: +Inf only
    let snap = h.snapshot();
    assert_eq!(snap.buckets[0], 0, "0.01 bucket must not see 0.03");
    assert_eq!(snap.buckets[1], 0, "0.025 bucket must not see 0.03");
    assert_eq!(snap.buckets[2], 2, "0.05 bucket sees both 0.03s");
    assert_eq!(
        snap.buckets[DURATION_BUCKETS_SECS.len() - 1],
        2,
        "last finite bucket must NOT include the 500s outlier"
    );
    assert_eq!(snap.count, 3, "+Inf sees all three");
    assert_eq!(snap.sum_micros, 30_000 + 30_000 + 500_000_000);
}

#[test]
fn sum_never_underflows_on_negative_clock() {
    let h = DurationHistogram::default();
    h.record(-1.0); // a clock that stepped backwards must clamp, not wrap
    assert_eq!(h.sum_micros.load(Ordering::Relaxed), 0);
    assert_eq!(h.snapshot().count, 1);
}
