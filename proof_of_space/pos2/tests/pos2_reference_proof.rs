use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_pos2::bits::expand_bits;
use dg_xch_pos2::constants::TOTAL_XS_IN_PROOF;
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::validator::ProofValidator;

/// A k18 strength2 proof for the plot id and challenge below, with the quality chain it yields.
const PROOF_HEX: &str = concat!(
    "e1f2a972453be5d7b941ab6af512678cfd2eb98fd6203ce22e194e593b6fa2a1",
    "2d712e6f4c4ef358ec91fd86e5a735525e999ab8bd9114cfa8c9f1092e904f57",
    "83b180d5939ff7eb7fb6824d654cae8a219b6499356f790b2a88e38ba3c6e0bc",
    "8d3dc6dd77dc60aa65a178845613bc89ab3cd732788b1d2c405b738548d6bce2",
    "ea9203caa8b958d345f0bc5ff4a60e7e5745f192552b1c6b347a62ab52628c47",
    "b4282e0ecaa0c8d4ba3d18654332fdb97b2aea25fe142bdd82b389f2b711e086",
    "be41c97ec9e0e826780b6f35021dd2291f007e40d91edecd382feff1113b963d",
    "4105d774ee5092549f2f00c84d9b90ba60c111d9bbfb7f7d34df9acf4a454427",
    "98636706659d7c0ef05e8c866b53deacbbf6a6bd509e7af64fea03bd598e4395",
);

const EXPECTED_FRAGMENTS: [u64; 16] = [
    25_078_806_449,
    118_437_799,
    50_946_205_485,
    27_200_007_405,
    25_066_125_455,
    125_785_320,
    50_940_638_819,
    27_210_765_036,
    25_075_426_666,
    119_333_627,
    50_943_925_885,
    27_208_104_175,
    25_065_941_201,
    133_143_305,
    50_944_978_770,
    27_201_925_752,
];

fn hex32(s: &str) -> Bytes32 {
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("hex");
    }
    Bytes32::from(out)
}

fn proof_bytes() -> Vec<u8> {
    (0..PROOF_HEX.len() / 2)
        .map(|i| u8::from_str_radix(&PROOF_HEX[i * 2..i * 2 + 2], 16).expect("hex"))
        .collect()
}

fn validator() -> ProofValidator {
    let plot_id = hex32("0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF");
    ProofValidator::new(ProofParams::new(plot_id, 18, 2, false).expect("params"))
        .expect("validator")
}

fn challenge() -> Bytes32 {
    hex32("0100000000000000000000000000000000000000000000000000000000000000")
}

fn proof_xs() -> [u32; TOTAL_XS_IN_PROOF] {
    let bytes = proof_bytes();
    assert_eq!(bytes.len(), 288, "a k18 proof is 288 bytes");
    let xs = expand_bits(&bytes, 18).expect("proof expands");
    assert_eq!(xs.len(), TOTAL_XS_IN_PROOF);
    xs.try_into().expect("128 x values")
}

#[test]
fn the_reference_proof_validates_and_yields_its_quality_chain() {
    let fragments = validator()
        .validate_full_proof(&proof_xs(), challenge())
        .expect("the reference proof must validate");
    assert_eq!(fragments, EXPECTED_FRAGMENTS);
}

#[test]
fn every_sub_proof_pairs_through_all_three_tables() {
    let v = validator();
    let xs = proof_xs();
    for i in 0..16 {
        let mut group = [0u32; 8];
        group.copy_from_slice(&xs[i * 8..i * 8 + 8]);
        assert!(
            v.validate_table_3_pairs(&group).is_some(),
            "sub proof {i} failed to pair"
        );
    }
}

#[test]
fn the_reference_proof_is_bound_to_its_challenge() {
    let v = validator();
    let xs = proof_xs();
    // The sub proofs still pair, but the chain no longer holds against a different challenge.
    let other = hex32("0200000000000000000000000000000000000000000000000000000000000000");
    assert!(v.validate_full_proof(&xs, other).is_none());
}

#[test]
fn a_perturbed_reference_proof_is_rejected() {
    let v = validator();
    let mut xs = proof_xs();
    xs[0] ^= 1;
    assert!(v.validate_full_proof(&xs, challenge()).is_none());
}

#[test]
fn packed_proofs_require_exact_wire_length() {
    let validator = validator();
    let mut proof = proof_bytes();
    assert_eq!(
        validator.validate_packed_proof(&proof, challenge()),
        Some(EXPECTED_FRAGMENTS)
    );
    proof.push(0);
    assert!(
        validator
            .validate_packed_proof(&proof, challenge())
            .is_none()
    );
    proof.truncate(287);
    assert!(
        validator
            .validate_packed_proof(&proof, challenge())
            .is_none()
    );
    assert!(validator.validate_packed_proof(&[], challenge()).is_none());
}

#[test]
fn unpacked_values_must_fit_k_bits() {
    let mut proof = proof_xs();
    proof[0] |= 1 << 18;
    assert!(
        validator()
            .validate_full_proof(&proof, challenge())
            .is_none()
    );
    assert!(
        validator()
            .validate_table_1_pair(&[proof[0], proof[1]])
            .is_none()
    );
}

#[test]
fn parameter_domain_matches_reference_api() {
    for k in 0..=u8::MAX {
        assert_eq!(
            ProofParams::new([0; 32].into(), k, 2, false).is_ok(),
            (18..=32).contains(&k) && k % 2 == 0
        );
    }
}

#[test]
fn native_search_recovers_reference_chain_and_enforces_limits() {
    use dg_xch_pos2::chainer::{Chainer, SearchError, SearchLimits};
    use std::sync::atomic::{AtomicBool, Ordering};
    let validator = validator();
    let core = validator.core();
    let ranges = core.select_challenge_sets(challenge()).ranges;
    let sets: [Vec<u64>; 4] = std::array::from_fn(|index| {
        let mut values: Vec<_> = EXPECTED_FRAGMENTS
            .iter()
            .copied()
            .filter(|value| ranges[index].contains(*value))
            .collect();
        values.sort_unstable();
        values.dedup();
        values
    });
    let slices = std::array::from_fn(|index| sets[index].as_slice());
    let cancelled = AtomicBool::new(false);
    let chainer = Chainer::new(core, challenge());
    let limits = SearchLimits {
        max_hashes: 1_000_000,
        max_results: 100,
    };
    let chains = chainer.search(slices, limits, &cancelled).unwrap();
    assert!(
        chains
            .iter()
            .any(|chain| chain.fragments == EXPECTED_FRAGMENTS)
    );
    assert!(chains.iter().all(|chain| chainer.validate(chain, &ranges)));
    assert_eq!(
        chainer.search(
            slices,
            SearchLimits {
                max_hashes: 0,
                ..limits
            },
            &cancelled
        ),
        Err(SearchError::WorkLimit)
    );
    assert_eq!(
        chainer.search(
            slices,
            SearchLimits {
                max_results: 0,
                ..limits
            },
            &cancelled
        ),
        Err(SearchError::ResultLimit)
    );
    cancelled.store(true, Ordering::Relaxed);
    assert_eq!(
        chainer.search(slices, limits, &cancelled),
        Err(SearchError::Cancelled)
    );
    let invalid = [u64::MAX];
    assert_eq!(
        chainer.search([&invalid, &[], &[], &[]], limits, &cancelled),
        Err(SearchError::InvalidSets)
    );
}

#[test]
#[ignore = "constructs the full native k18 reference plot"]
fn native_plot_contains_every_reference_fragment_and_recovers_the_chain() {
    use dg_xch_pos2::{
        chainer::SearchLimits,
        plotting::{NativePlot, PlotLimits},
    };
    use std::sync::atomic::AtomicBool;
    let cancelled = AtomicBool::new(false);
    let plot = NativePlot::build(
        validator().core().params().clone(),
        PlotLimits::default(),
        &cancelled,
    )
    .unwrap();
    for fragment in EXPECTED_FRAGMENTS {
        assert!(
            plot.witnesses()
                .binary_search_by_key(&fragment, |witness| witness.fragment)
                .is_ok()
        );
    }
    let chains = plot
        .qualities(
            challenge(),
            SearchLimits {
                max_hashes: 10_000_000,
                max_results: 1024,
            },
            &cancelled,
        )
        .unwrap();
    let chain = chains
        .iter()
        .find(|chain| chain.fragments == EXPECTED_FRAGMENTS)
        .expect("reference chain recovered natively");
    let proof = plot.prove(chain, challenge()).unwrap();
    assert_eq!(
        validator().validate_packed_proof(&proof, challenge()),
        Some(EXPECTED_FRAGMENTS)
    );
}

#[test]
fn cuda_shared_pairing_and_scan_logic_recovers_reference_fragments() {
    use dg_xch_pos2::device;
    let validator = validator();
    let config = device::Config {
        plot_id: *AsRef::<[u8; 32]>::as_ref(&validator.core().params().plot_id()),
        k: 18,
        strength: 2,
        testnet: 0,
    };
    let values = proof_xs();
    for (group, expected) in values.as_chunks::<8>().0.iter().zip(EXPECTED_FRAGMENTS) {
        let mut entries: Vec<_> = group
            .iter()
            .map(|value| device::generate(config, *value))
            .collect();
        for table in 1..=3 {
            let mut next = Vec::new();
            for pair in entries.as_chunks::<2>().0 {
                let combined = device::pair(config, table, pair[0], pair[1]);
                assert_eq!(combined.valid, 1);
                let mut sorted = entries.clone();
                sorted.sort_unstable_by_key(|entry| (entry.info, entry.meta, entry.xs));
                let left = sorted
                    .iter()
                    .position(|entry| entry.xs == pair[0].xs)
                    .unwrap();
                let (_, count) =
                    device::scan(config, table, &sorted, sorted.len(), left, u32::MAX, 1000);
                assert!(count > 0 && count < 1000);
                assert!((0..count).any(|wanted| {
                    device::scan(config, table, &sorted, sorted.len(), left, wanted, 1000).0
                        == combined
                }));
                assert_eq!(
                    device::scan(config, table, &sorted, sorted.len(), left, u32::MAX, 0).1,
                    u32::MAX
                );
                next.push(combined);
            }
            entries = next;
        }
        assert_eq!(entries[0].fragment, expected);
        assert_eq!(entries[0].xs.as_slice(), group);
    }
}
