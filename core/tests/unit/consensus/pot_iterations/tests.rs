//! Test constants override `NUM_SPS_SUB_SLOT=32`, so the overflow boundary is
//! `32 - NUM_SP_INTERVALS_EXTRA(3) = 29`.
//!
//! `calculate_iterations_quality` keeps the v1 `size: u8` signature; versions are routed
//! through `calculate_iterations_quality_for_proof`, and the v2 vectors run against
//! `expected_plot_size_v2` / `calculate_iterations_quality_v2`.
//!
//! An astronomically-large quotient clamps to `u64::MAX` (see
//! `clamps_to_u64_max_on_overflow`); the impossible proof is still rejected downstream, where
//! `calculate_ip_iters` errors on `required_iters >= sp_interval_iters`.

use super::*;
use crate::consensus::constants::MAINNET;
use crate::traits::SizedBytes; // Bytes32::new (quality-hash construction in the harvested vectors)

// ConsensusConstants is Copy.
fn test_constants() -> ConsensusConstants {
    ConsensusConstants {
        num_sps_sub_slot: 32,
        ..MAINNET
    }
}

#[test]
fn test_is_overflow_block() {
    let c = test_constants();
    assert!(!is_overflow_block(&c, 27).unwrap());
    assert!(!is_overflow_block(&c, 28).unwrap());
    assert!(is_overflow_block(&c, 29).unwrap());
    assert!(is_overflow_block(&c, 30).unwrap());
    assert!(is_overflow_block(&c, 31).unwrap());
    let err = is_overflow_block(&c, 32).unwrap_err();
    assert!(err.to_string().contains("SP index too high"));
}

#[test]
fn test_calculate_sp_iters() {
    let c = test_constants();
    let ssi: u64 = 100_001 * 64 * 4;
    // index == NUM_SPS_SUB_SLOT errors
    let err = calculate_sp_iters(&c, ssi, 32).unwrap_err();
    assert!(err.to_string().contains("SP index too high"));
    // The last valid index (31) does not error.
    assert!(calculate_sp_iters(&c, ssi, 31).is_ok());
}

#[test]
fn test_calculate_ip_iters() {
    let c = test_constants();
    let ssi: u64 = 100_001 * 64 * 4;
    let sp_interval_iters = ssi / u64::from(c.num_sps_sub_slot);
    let extra = c.num_sp_intervals_extra;

    // invalid signage point index -> "SP index too high"
    let err = calculate_ip_iters(&c, ssi, 123, 100_000).unwrap_err();
    assert!(err.to_string().contains("SP index too high"));

    let sp_iters = sp_interval_iters * 13;

    // required_iters too high (== and > sp_interval_iters) errors
    let err = calculate_ip_iters(&c, ssi, 0, sp_interval_iters).unwrap_err();
    assert!(err.to_string().contains("Required iters"));
    let err = calculate_ip_iters(&c, ssi, 0, sp_interval_iters * 12).unwrap_err();
    assert!(err.to_string().contains("Required iters"));

    // required_iters too low (0) -> same error
    let err = calculate_ip_iters(&c, ssi, 0, 0).unwrap_err();
    assert!(err.to_string().contains("Required iters"));

    // Non-overflow: ip_iters == sp_iters + extra*sp_interval + required (the % ssi is a no-op).
    let required_iters = sp_interval_iters - 1;
    let ip_iters = calculate_ip_iters(&c, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + extra * sp_interval_iters + required_iters
    );

    let required_iters = 1;
    let ip_iters = calculate_ip_iters(&c, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + extra * sp_interval_iters + required_iters
    );

    // required_iters = ssi * 4 / 300 (integer division)
    let required_iters = (ssi * 4) / 300;
    let ip_iters = calculate_ip_iters(&c, ssi, 13, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        sp_iters + extra * sp_interval_iters + required_iters
    );
    assert!(sp_iters < ip_iters);

    // Overflow: index NUM_SPS_SUB_SLOT-1, sp_iters > ip_iters,
    // ip_iters == (sp_iters + extra*sp_interval + required) % ssi.
    let sp_iters = sp_interval_iters * u64::from(c.num_sps_sub_slot - 1);
    let ip_iters =
        calculate_ip_iters(&c, ssi, (c.num_sps_sub_slot - 1) as u8, required_iters).unwrap();
    assert_eq!(
        ip_iters,
        (sp_iters + extra * sp_interval_iters + required_iters) % ssi
    );
    assert!(sp_iters > ip_iters);
}

// Deterministic invariants for the v1 quality -> required_iters path: floored at 1,
// linear in difficulty, inverse in plot size.
#[test]
fn calculate_iterations_quality_v1_invariants() {
    let dcf = MAINNET.difficulty_constant_factor;
    let q = Bytes32::from([7u8; 32]);
    let sp = Bytes32::from([9u8; 32]);
    // Always >= 1.
    assert!(calculate_iterations_quality(dcf, q, 32, 1, sp) >= 1);
    // Linear in difficulty => monotonic non-decreasing.
    let low = calculate_iterations_quality(dcf, q, 32, 1, sp);
    let high = calculate_iterations_quality(dcf, q, 32, 1_000, sp);
    assert!(high >= low, "required_iters grows with difficulty");
    // Inverse in expected_plot_size => a larger k yields fewer-or-equal iters.
    let small_k = calculate_iterations_quality(dcf, q, 32, 1_000_000, sp);
    let large_k = calculate_iterations_quality(dcf, q, 40, 1_000_000, sp);
    assert!(
        large_k <= small_k,
        "a bigger plot wins more often (fewer iters)"
    );
}

// DIVERGENCE lock: dg_xch clamps to u64::MAX instead of erroring on an oversized quotient.
#[test]
fn clamps_to_u64_max_on_overflow() {
    let got = calculate_iterations_quality(
        u128::MAX,
        Bytes32::from([0xFFu8; 32]),
        18,
        u64::MAX,
        Bytes32::from([0xFFu8; 32]),
    );
    assert_eq!(got, u64::MAX);
}

#[test]
fn test_expected_plot_size_v1() {
    let mut last_size = 2_400_000u64;
    for k in 18u8..50 {
        let plot_size = expected_plot_size(k);
        assert!(plot_size > last_size * 2, "k={k} not > 2x previous");
        last_size = plot_size;
    }
}

// Five v1 farmer classes and three v2 classes. The v2 classes share the network's fixed v2
// plot size, so they hold identical space and win identically; strength plays no part in the
// lottery. A ~400k-iteration probabilistic vector with a 1% tolerance.
#[test]
fn test_win_percentage() {
    struct FarmerClass {
        version: u8,
        k: u8,
        count: u64,
        space: u128,
        wins: u64,
    }
    let constants = ConsensusConstants {
        num_sps_sub_slot: 32,
        difficulty_constant_factor: 2u128.pow(25),
        ..MAINNET
    };
    let v2_size = u128::from(expected_plot_size_v2(constants.plot_size_v2));
    let mut classes: Vec<FarmerClass> = [32u8, 33, 34, 35, 36]
        .into_iter()
        .map(|k| FarmerClass {
            version: 0,
            k,
            count: 100,
            space: u128::from(expected_plot_size(k)) * 100,
            wins: 0,
        })
        .chain((0..3).map(|_| FarmerClass {
            version: 1,
            k: constants.plot_size_v2,
            count: 200,
            space: v2_size * 200,
            wins: 0,
        }))
        .collect();

    let total_slots = 50u32;
    let num_sps = 16u32;
    let sub_slot_iters: u64 = 100_000_000;
    let sp_interval_iters = calculate_sp_interval_iters(&constants, sub_slot_iters).unwrap();
    let difficulty: u64 = 500_000_000_000;

    for slot_index in 0..total_slots {
        for sp_index in 0..num_sps {
            let mut sp_in = Vec::new();
            sp_in.extend_from_slice(&slot_index.to_be_bytes());
            sp_in.extend_from_slice(&sp_index.to_be_bytes());
            let sp_hash = Bytes32::new(hash_256(sp_in));
            for class in &mut classes {
                for farmer_index in 0..class.count {
                    // std_hash(slot_be4 + k_1byte + farmer_index zero bytes).
                    let mut q_in = Vec::new();
                    q_in.extend_from_slice(&slot_index.to_be_bytes());
                    q_in.push(class.k);
                    let base = q_in.len();
                    q_in.resize(base + farmer_index as usize, 0u8);
                    let quality = Bytes32::new(hash_256(q_in));
                    let required_iters = if class.version == 0 {
                        calculate_iterations_quality(
                            constants.difficulty_constant_factor,
                            quality,
                            class.k,
                            difficulty,
                            sp_hash,
                        )
                    } else {
                        calculate_iterations_quality_v2(
                            constants.difficulty_constant_factor,
                            quality,
                            constants.plot_size_v2,
                            difficulty,
                            sp_hash,
                        )
                    };
                    if required_iters < sp_interval_iters {
                        class.wins += 1;
                    }
                }
            }
        }
    }

    let total_space: u128 = classes.iter().map(|c| c.space).sum();
    let total_wins: u64 = classes.iter().map(|c| c.wins).sum();
    for class in &classes {
        let percentage_space = class.space as f64 / total_space as f64;
        let win_percentage = class.wins as f64 / total_wins as f64;
        assert!(
            (win_percentage - percentage_space).abs() < 0.01,
            "v{} k={}: win {win_percentage} vs space {percentage_space}",
            class.version,
            class.k
        );
    }
    // The three v2 classes are indistinguishable to the lottery by construction.
    assert_eq!(classes[5].wins, classes[6].wins);
    assert_eq!(classes[6].wins, classes[7].wins);
}

// The v2 size is one constant, blind to strength, plot index and group.
#[test]
fn test_expected_plot_size_v2() {
    let c = ConsensusConstants {
        num_sps_sub_slot: 32,
        ..MAINNET
    };
    assert_eq!(expected_plot_size_v2(c.plot_size_v2), 988_513_566);
}

#[test]
fn v2_expected_plot_size_matches_the_reference_float_math() {
    // int((2**k) * (k + 1.46) / 8), in IEEE754 f64.
    assert_eq!(super::expected_plot_size_v2(28), 988_513_566);
    assert_eq!(super::expected_plot_size_v2(18), 637_665);
    assert_eq!(super::expected_plot_size_v2(30), 4_222_489_722);
}

#[test]
fn v2_iterations_scale_inversely_with_plot_size() {
    use crate::blockchain::sized_bytes::Bytes32;
    let q = Bytes32::from([7u8; 32]);
    let sp = Bytes32::from([9u8; 32]);
    let small = super::calculate_iterations_quality_v2(2u128.pow(67), q, 18, 1000, sp);
    let big = super::calculate_iterations_quality_v2(2u128.pow(67), q, 28, 1000, sp);
    // A larger plot wins the same quality draw with fewer required iterations.
    assert!(big < small, "big {big} !< small {small}");
    assert!(small >= 1 && big >= 1);
    // The v1 and v2 size models are different functions, even at the same k.
    assert_ne!(
        super::calculate_iterations_quality(2u128.pow(67), q, 28, 1000, sp),
        big
    );
}
