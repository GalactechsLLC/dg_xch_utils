use super::EscalationBackoff;

/// Drive one over-trigger tick; returns whether an attempt ran, feeding `drained` back
/// when it did.
fn tick(b: &mut EscalationBackoff, drained: bool) -> bool {
    if b.should_attempt() {
        b.record(drained);
        true
    } else {
        false
    }
}

#[test]
fn first_attempt_is_immediate() {
    let mut b = EscalationBackoff::new();
    assert!(b.should_attempt());
}

#[test]
fn failures_space_attempts_exponentially_to_the_cap() {
    let mut b = EscalationBackoff::new();
    // Simulate a pinned reader: every attempt fails to drain. Collect the gap (in
    // skipped ticks) before each of the next attempts.
    assert!(tick(&mut b, false), "first attempt must be immediate");
    let mut gaps = Vec::new();
    let mut skipped = 0u32;
    while gaps.len() < 8 {
        if tick(&mut b, false) {
            gaps.push(skipped);
            skipped = 0;
        } else {
            skipped += 1;
        }
    }
    assert_eq!(
        gaps,
        vec![2, 4, 8, 16, 32, 64, 64, 64],
        "retries must double up to the cap and then hold it"
    );
}

#[test]
fn a_successful_drain_resets_to_immediate() {
    let mut b = EscalationBackoff::new();
    for _ in 0..40 {
        tick(&mut b, false);
    }
    // A drain that works clears the accumulated width entirely.
    loop {
        if tick(&mut b, true) {
            break;
        }
    }
    assert!(b.should_attempt(), "post-success attempt must be immediate");
}

#[test]
fn dropping_under_the_trigger_resets() {
    let mut b = EscalationBackoff::new();
    for _ in 0..40 {
        tick(&mut b, false);
    }
    b.reset();
    assert!(
        b.should_attempt(),
        "under-trigger reset must clear the backoff"
    );
}
