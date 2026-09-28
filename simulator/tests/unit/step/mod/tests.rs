use super::*;

#[test]
fn emitted_timestamps_are_strictly_increasing() {
    let mut e = TimestampEmitter::new(1_000);
    let mut last = 1_000;
    for _ in 0..100 {
        e.advance(0.01);
        let ts = e.emit();
        assert!(ts > last, "{ts} !> {last}");
        last = ts;
    }
}

#[test]
fn quantizing_only_at_emission_avoids_drift() {
    let mut e = TimestampEmitter::new(0);
    for _ in 0..1_000 {
        e.advance(18.6);
        e.emit();
    }
    // Rounding at every block would lose up to 0.6s each, roughly 600s over this run.
    assert_eq!(e.anchor(), 18_600);
}

#[test]
fn many_small_advances_match_one_large_advance() {
    let mut split = TimestampEmitter::new(500);
    for _ in 0..64 {
        split.advance(0.25);
    }
    let mut whole = TimestampEmitter::new(500);
    whole.advance(16.0);
    assert_eq!(split.emit(), whole.emit());
}

#[test]
fn a_borrowed_second_is_repaid() {
    let mut e = TimestampEmitter::new(0);
    // Four blocks arrive inside one second, then the chain idles.
    for _ in 0..4 {
        e.advance(0.25);
        e.emit();
    }
    assert_eq!(e.anchor(), 4);
    e.advance(4.0);
    assert_eq!(e.emit(), 5);
}

#[test]
fn a_reorg_seed_never_equals_the_mainline_seed() {
    for run_seed in 0..256u64 {
        for fork_ordinal in 0..8u64 {
            assert_ne!(reorg_seed(run_seed, fork_ordinal), run_seed);
        }
    }
}

#[test]
fn reorg_seeds_are_deterministic_and_distinct_per_fork() {
    assert_eq!(reorg_seed(7, 0), reorg_seed(7, 0));
    assert_ne!(reorg_seed(7, 0), reorg_seed(7, 1));
    assert_ne!(reorg_seed(7, 0), reorg_seed(8, 0));
}
