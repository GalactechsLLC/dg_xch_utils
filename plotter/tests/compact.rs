use dg_xch_pos2::{
    compact::CompactPlot,
    compute::{CpuHasher, HashEngine, Work},
    params::ProofParams,
    plotting::{NativePlot, PlotLimits},
};
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "constructs full low-k plots; run in release mode"]
fn compact_tables_match_full_witness_pipeline() {
    let cancelled = AtomicBool::new(false);
    for testnet in [false, true] {
        for strength in [2, 3] {
            let params = ProofParams::new([7; 32].into(), 18, strength, testnet).unwrap();
            let compact =
                CompactPlot::build(params.clone(), PlotLimits::default(), &cancelled).unwrap();
            let sparse = CompactPlot::build(
                params.clone(),
                PlotLimits {
                    memory_bytes: CompactPlot::memory_required(
                        18,
                        PlotLimits::default().max_entries,
                    )
                    .unwrap(),
                    ..PlotLimits::default()
                },
                &cancelled,
            )
            .unwrap();
            assert_eq!(sparse.fragments(), compact.fragments());
            let witnesses = NativePlot::build(params, PlotLimits::default(), &cancelled).unwrap();
            assert_eq!(compact.table_counts, witnesses.table_counts);
            assert!(
                compact
                    .fragments()
                    .iter()
                    .copied()
                    .eq(witnesses.witnesses().iter().map(|witness| witness.fragment))
            );
        }
    }
}

#[test]
fn all_reference_sizes_and_strengths_have_checked_memory_plans() {
    for k in (18..=32).step_by(2) {
        for strength in 2..=k - if k < 28 { 2 } else { k - 26 } - 1 {
            let params = ProofParams::new([0; 32].into(), k, strength, false).unwrap();
            assert!(CompactPlot::minimum_work(&params) > (1u64 << k));
            assert!(
                CompactPlot::build(
                    params,
                    PlotLimits {
                        memory_bytes: u64::MAX,
                        max_entries: usize::MAX,
                        max_work: 1
                    },
                    &AtomicBool::new(false)
                )
                .is_err()
            );
        }
        assert!(CompactPlot::memory_required(k, usize::MAX).unwrap() > (1u64 << k));
    }
    assert!(CompactPlot::memory_required(28, 100).is_err());
    assert!(CompactPlot::memory_required(28, usize::MAX).unwrap() < 10 * 1024 * 1024 * 1024);
}

#[test]
fn extended_strength_hashing_is_split_without_changing_results() {
    let params = ProofParams::new([3; 32].into(), 18, 9, false).unwrap();
    let input = [[1, 2, 3, 4]];
    let cancelled = AtomicBool::new(false);
    let mut engine = CpuHasher::new(&params);
    let expected = engine.hash(&input, 1024, &cancelled).unwrap();
    let expected = engine.hash(&expected, 1024, &cancelled).unwrap();
    let mut work = Work::new(PlotLimits::default(), &cancelled);
    assert_eq!(work.hash(&mut engine, &input, 2048).unwrap(), expected);
    assert!(
        Work::new(
            PlotLimits {
                max_work: 1,
                ..PlotLimits::default()
            },
            &cancelled
        )
        .hash(&mut engine, &input, 2048)
        .is_err()
    );
}

#[test]
#[ignore = "constructs and solves low-k plots; run in release mode"]
fn stored_fragments_reconstruct_valid_proofs() {
    use blst::min_pk::SecretKey;
    use dg_xch_plotter::{PlotRequest, PoolBinding, reader::PlotReader};
    use dg_xch_pos2::{chainer::SearchLimits, validator::ProofValidator};
    let cancelled = AtomicBool::new(false);
    for testnet in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("compact.plot");
        let request = PlotRequest {
            farmer_public_key: SecretKey::key_gen_v3(&[7; 32], &[])
                .unwrap()
                .sk_to_pk()
                .to_bytes(),
            pool: PoolBinding::Contract([8; 32]),
            k: 18,
            strength: 2,
            index: 256,
            meta_group: 3,
            testnet,
        };
        let limits = PlotLimits::default();
        let info = dg_xch_plotter::create(&request, &path, limits, &cancelled).unwrap();
        let params = ProofParams::new(info.plot_id.into(), info.k, info.strength, testnet).unwrap();
        let native = NativePlot::build(params.clone(), limits, &cancelled).unwrap();
        let mut reader = PlotReader::open(&path, testnet, limits.memory_bytes).unwrap();
        let search = SearchLimits {
            max_hashes: 10_000_000,
            max_results: 1024,
        };
        let validator = ProofValidator::new(params).unwrap();
        let mut found = false;
        for attempt in 0u16..256 {
            let mut challenge = [0; 32];
            challenge[..2].copy_from_slice(&attempt.to_le_bytes());
            let challenge = challenge.into();
            let expected = native.qualities(challenge, search, &cancelled).unwrap();
            let actual = reader.qualities(challenge, search, &cancelled).unwrap();
            assert_eq!(
                actual
                    .iter()
                    .map(|chain| chain.fragments)
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|chain| chain.fragments)
                    .collect::<Vec<_>>()
            );
            if let Some(chain) = actual.first() {
                let proof = reader.prove(chain, challenge, limits, &cancelled).unwrap();
                assert_eq!(
                    validator.validate_packed_proof(&proof, challenge),
                    Some(chain.fragments)
                );
                assert!(
                    reader
                        .prove(chain, challenge, limits, &AtomicBool::new(true))
                        .is_err()
                );
                found = true;
                break;
            }
        }
        assert!(found);
    }
}

#[test]
#[ignore = "constructs multiple in-memory plots; run in release mode"]
fn in_memory_plots_support_both_memos_and_network_domains() {
    use blst::min_pk::SecretKey;
    use dg_xch_plotter::{
        PlotRequest, PoolBinding, create_in_memory, inspect_reader, reader::PlotReader,
    };
    use dg_xch_pos2::params::Range;
    use std::io::Cursor;
    let cancelled = AtomicBool::new(false);
    let key = SecretKey::key_gen_v3(&[11; 32], &[])
        .unwrap()
        .sk_to_pk()
        .to_bytes();
    for portable in [false, true] {
        for testnet in [false, true] {
            let request = PlotRequest {
                farmer_public_key: key,
                pool: if portable {
                    PoolBinding::Contract([9; 32])
                } else {
                    PoolBinding::PublicKey(key)
                },
                k: 18,
                strength: 2,
                index: u16::MAX,
                meta_group: u8::MAX,
                testnet,
            };
            let (info, bytes) =
                create_in_memory(&request, PlotLimits::default(), &cancelled).unwrap();
            assert_eq!(info, inspect_reader(&mut Cursor::new(&bytes)).unwrap());
            assert_eq!(info.portable, portable);
            let mut reader =
                PlotReader::from_reader(Cursor::new(&bytes), testnet, 512 * 1024 * 1024).unwrap();
            let params = ProofParams::new(info.plot_id.into(), 18, 2, testnet).unwrap();
            let plot = CompactPlot::build(params, PlotLimits::default(), &cancelled).unwrap();
            for fragment in plot.fragments().iter().copied().step_by(10_000) {
                assert_eq!(
                    reader
                        .fragments_in_range(
                            Range {
                                start: fragment,
                                end: fragment
                            },
                            &cancelled
                        )
                        .unwrap(),
                    vec![fragment]
                );
            }
            assert!(
                create_in_memory(&request, PlotLimits::default(), &AtomicBool::new(true)).is_err()
            );
            if !portable && !testnet {
                let directory = 43 + 128 + 8;
                let offset = u64::from_le_bytes(bytes[directory..directory + 8].try_into().unwrap())
                    as usize;
                for position in 0..128 {
                    let mut corrupted = bytes.clone();
                    corrupted[offset + 20 + position] ^= 0xff;
                    let mut reader =
                        PlotReader::from_reader(Cursor::new(corrupted), testnet, 512 * 1024 * 1024)
                            .unwrap();
                    let _ = reader.fragments_in_range(
                        Range {
                            start: 0,
                            end: (1 << 34) - 1,
                        },
                        &cancelled,
                    );
                }
            }
        }
    }
}
