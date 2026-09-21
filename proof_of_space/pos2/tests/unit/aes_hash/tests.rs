use super::*;

/// The regression plot id: `plot_id[i] = i * 11 + 5`, used at k28.
fn regression_plot_id() -> Bytes32 {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    Bytes32::from(bytes)
}

/// The regression inputs, in the order the vector below records them.
fn regression_results(hasher: &AesHash) -> Vec<u32> {
    let mut out = Vec::with_capacity(41);
    for x in [0u32, 1, 0x1234_5678, 0xFFFF_FFFF, 0xABCD_EF12] {
        out.push(hasher.g_x(x, AES_G_ROUNDS));
    }
    for extra_bits in [0u32, 1] {
        for meta in [0u64, 0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210] {
            out.push(hasher.matching_target(1, 0xDEAD_BEEF, meta, extra_bits));
            out.push(hasher.matching_target(3, 0x0123_ABCD, meta, extra_bits));
        }
    }
    for extra_bits in [0u32, 1] {
        for (l, r) in [
            (0x0123_4567_89AB_CDEFu64, 0x0FED_CBA9_8765_4321u64),
            (0, 0),
            (0xFFFF_FFFF_FFFF_FFFF, 0xAAAA_AAAA_AAAA_AAAA),
        ] {
            out.extend_from_slice(&hasher.pairing(l, r, extra_bits));
        }
    }
    out
}

/// The frozen regression vector.
const AES_REGRESSION: [u32; 41] = [
    127_783_594,
    124_767_263,
    141_145_580,
    258_627_148,
    11_512_430,
    409_340_410,
    505_582_378,
    1_721_018_924,
    1_480_140_290,
    1_396_524_303,
    3_587_190_239,
    707_841_484,
    1_331_347_346,
    1_241_879_891,
    2_167_726_916,
    3_067_597_756,
    4_168_169_983,
    1_091_708_360,
    2_175_255_815,
    2_816_383_768,
    1_674_980_125,
    2_543_702_698,
    4_091_426_003,
    533_075_521,
    3_859_141_200,
    31_044_209,
    4_179_457_918,
    1_030_061_401,
    3_699_883_668,
    210_961_197,
    1_476_679_550,
    3_006_735_961,
    939_518_466,
    1_218_571_309,
    716_491_999,
    1_747_602_127,
    1_064_749_683,
    1_584_340_891,
    3_071_410_499,
    4_118_871_486,
    1_400_922_689,
];

#[test]
fn the_hash_matches_the_reference_regression_vector() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    let got = regression_results(&hasher);
    assert_eq!(got.len(), AES_REGRESSION.len());
    for (i, (got, want)) in got.iter().zip(AES_REGRESSION.iter()).enumerate() {
        assert_eq!(got, want, "regression value {i} diverged");
    }
}

#[test]
fn the_native_and_portable_paths_agree() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    for x in [0u32, 1, 0x1234_5678, 0xFFFF_FFFF] {
        let state = AesHash::state(x, 0, 0, 0);
        assert_eq!(
            hasher.apply(state, AES_G_ROUNDS),
            hasher.apply_portable(state, AES_G_ROUNDS),
            "native and portable diverged for x {x}"
        );
    }
}

#[test]
fn the_batch_and_scalar_paths_agree() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    // Deliberately not a multiple of the lane count, so the tail is exercised too.
    let xs: Vec<u32> = (0..37u32).map(|i| i.wrapping_mul(0x9E37_79B9)).collect();
    let mut out = vec![0u32; xs.len()];
    hasher.g_x_batch(&xs, &mut out, AES_G_ROUNDS);
    for (i, x) in xs.iter().enumerate() {
        assert_eq!(
            out[i],
            hasher.g_x(*x, AES_G_ROUNDS),
            "batch diverged at {i}"
        );
    }
}

#[test]
fn complete_state_batches_match_portable_and_scalar_paths() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    let inputs: Vec<[u32; 4]> = (0..37u32)
        .map(|index| {
            [
                index.wrapping_mul(0x9E37_79B9),
                index.wrapping_mul(0xCA01_F9DD),
                index.rotate_left(13) ^ 0xDEAD_BEEF,
                u32::MAX.wrapping_sub(index),
            ]
        })
        .collect();
    for length in [0, 1, 7, 8, 9, 16, 37] {
        for rounds in [1, 16, 32, 64, 1024] {
            let inputs = &inputs[..length];
            let mut actual = vec![[0; 4]; length];
            let mut portable = vec![[0; 4]; length];
            hasher.hash_words_batch(inputs, rounds, &mut actual);
            hasher.hash_words_batch_portable(inputs, rounds, &mut portable);
            assert_eq!(actual, portable, "length {length}, rounds {rounds}");
            for (input, actual) in inputs.iter().zip(actual) {
                let state = AesHash::state(input[0], input[1], input[2], input[3]);
                let expected = hasher.apply_portable(state, rounds);
                assert_eq!(
                    actual,
                    std::array::from_fn(|index| AesHash::lane(&expected, index)),
                    "length {length}, rounds {rounds}"
                );
            }
        }
    }
}

#[test]
fn serial_hashing_validates_output_length_and_cancellation() {
    use crate::compute::{CpuHasher, HashEngine};
    use crate::params::ProofParams;
    use std::sync::atomic::AtomicBool;

    let params = ProofParams::new(regression_plot_id(), 28, 2, false).unwrap();
    let mut hasher = CpuHasher::new(&params);
    let inputs: Vec<[u32; 4]> = (0..8193u32)
        .map(|index| [index, index.rotate_left(13), 17, u32::MAX])
        .collect();
    let cancelled = AtomicBool::new(false);
    let mut actual = vec![[0; 4]; inputs.len()];
    hasher
        .hash_into_serial(&inputs, 16, &mut actual, &cancelled)
        .unwrap();
    assert_eq!(actual, hasher.hash(&inputs, 16, &cancelled).unwrap());
    assert!(
        hasher
            .hash_into_serial(&inputs, 16, &mut actual[..3], &cancelled)
            .is_err()
    );
    for rounds in [0, 1025] {
        assert!(
            hasher
                .hash_into_serial(&inputs, rounds, &mut actual, &cancelled)
                .is_err()
        );
    }
    assert!(
        hasher
            .hash_into_serial(&inputs, 16, &mut actual, &AtomicBool::new(true))
            .is_err()
    );
}

#[test]
fn g_x_is_masked_to_k_bits() {
    for k in [16u8, 20, 28, 30] {
        let hasher = AesHash::new(&regression_plot_id(), k);
        for x in [0u32, 1, 0xDEAD_BEEF] {
            assert!(
                hasher.g_x(x, AES_G_ROUNDS) < (1u32 << k),
                "k {k} leaked bits above the mask"
            );
        }
    }
}

#[test]
fn extra_rounds_change_the_result() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    assert_ne!(
        hasher.matching_target(1, 7, 99, 0),
        hasher.matching_target(1, 7, 99, 1)
    );
    assert_ne!(hasher.pairing(1, 2, 0), hasher.pairing(1, 2, 1));
}

#[test]
fn the_keys_are_the_two_halves_of_the_plot_id() {
    let a = AesHash::new(&regression_plot_id(), 28);
    let mut swapped = regression_plot_id().bytes();
    swapped.swap(0, 16);
    let b = AesHash::new(&Bytes32::from(swapped), 28);
    assert_ne!(a.g_x(1, AES_G_ROUNDS), b.g_x(1, AES_G_ROUNDS));
}

#[test]
fn chaining_is_deterministic_and_input_sensitive() {
    let hasher = AesHash::new(&regression_plot_id(), 28);
    assert_eq!(hasher.chain(12345), hasher.chain(12345));
    assert_ne!(hasher.chain(12345), hasher.chain(12346));
}
