use super::*;
use crate::pos2::params::ProofParams;

fn core() -> ProofCore {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    ProofCore::new(ProofParams::new(Bytes32::from(bytes), 28, 2, false).expect("params"))
        .expect("core")
}

#[test]
fn the_last_link_threshold_matches_the_reference() {
    assert_eq!(*LAST_LINK_EXTRA_THRESHOLD, 3_127_297_797_138_110);
}

#[test]
fn the_correction_targets_one_chain_per_challenge() {
    // The threshold divides the 52 bit upper space by the ~1.44 bonus.
    let full = 2.0f64.powi(52);
    let ratio = full / (*LAST_LINK_EXTRA_THRESHOLD as f64);
    assert!(
        (1.43..1.45).contains(&ratio),
        "correction ratio was {ratio}"
    );
}

#[test]
fn each_link_asks_for_the_right_number_of_zero_bits() {
    assert_eq!(Chainer::zero_bits_needed(0), CHAIN_STARTER_FILTER_BITS);
    assert_eq!(Chainer::zero_bits_needed(1), CHAIN_SET_BITS);
    assert_eq!(
        Chainer::zero_bits_needed(NUM_CHAIN_LINKS - 2),
        CHAIN_SET_BITS
    );
    assert_eq!(
        Chainer::zero_bits_needed(NUM_CHAIN_LINKS - 1),
        CHAIN_SET_BITS + CHAIN_FACTOR_FRONT_LOAD_BITS
    );
}

#[test]
fn a_hash_with_the_wrong_low_bits_is_rejected() {
    for iteration in [0usize, 1, NUM_CHAIN_LINKS - 1] {
        let zeros = Chainer::zero_bits_needed(iteration);
        assert!(
            !Chainer::passes_fast_filter(1, iteration),
            "iter {iteration}"
        );
        assert!(
            Chainer::passes_fast_filter(0, iteration),
            "iter {iteration}"
        );
        let just_above = 1u64 << zeros;
        let passes = Chainer::passes_fast_filter(just_above, iteration);
        assert!(passes || iteration == NUM_CHAIN_LINKS - 1);
    }
}

#[test]
fn the_last_link_also_bounds_its_upper_bits() {
    let zeros = Chainer::zero_bits_needed(NUM_CHAIN_LINKS - 1);
    let below = (*LAST_LINK_EXTRA_THRESHOLD - 1) << zeros;
    let at = *LAST_LINK_EXTRA_THRESHOLD << zeros;
    assert!(Chainer::passes_fast_filter(below, NUM_CHAIN_LINKS - 1));
    assert!(!Chainer::passes_fast_filter(at, NUM_CHAIN_LINKS - 1));
    // An earlier link has no upper bound at all.
    assert!(Chainer::passes_fast_filter(at, 1));
}

#[test]
fn a_chain_outside_the_selected_ranges_is_rejected() {
    let core = core();
    let sets = core.select_challenge_sets(Bytes32::from([7u8; 32]));
    let chainer = Chainer::new(&core, Bytes32::from([7u8; 32]));
    // A fragment in none of the four ranges cannot start a chain.
    let outside = u64::MAX;
    assert!(!sets.ranges.iter().any(|r| r.contains(outside)));
    let chain = Chain {
        fragments: [outside; NUM_CHAIN_LINKS],
    };
    assert!(!chainer.validate(&chain, &sets.ranges));
}

#[test]
fn a_chain_that_stays_in_one_set_is_rejected() {
    // Links must rotate through the sets, so sixteen fragments from the starting set alone
    // cannot form a chain even though each one is individually in range.
    let core = core();
    let challenge = Bytes32::from([11u8; 32]);
    let sets = core.select_challenge_sets(challenge);
    let chainer = Chainer::new(&core, challenge);
    let chain = Chain {
        fragments: [sets.ranges[0].start; NUM_CHAIN_LINKS],
    };
    assert!(!chainer.validate(&chain, &sets.ranges));
}
