#![cfg(feature = "vulkan")]

use blst::min_pk::SecretKey;
use dg_xch_plotter::{PlotRequest, PoolBinding, create_with_engine, proving::ReconstructedPlot};
use dg_xch_pos2::{
    params::ProofParams,
    plotting::{NativePlot, PlotLimits},
    vulkan,
};
use std::sync::atomic::AtomicBool;

fn device_ordinal() -> usize {
    let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
        .map(|value| value.parse::<usize>().expect("device ordinal"))
        .unwrap_or(0);
    eprintln!(
        "Vulkan test device: {:?}",
        vulkan::adapters().get(ordinal).unwrap()
    );
    ordinal
}

#[test]
#[ignore = "requires hardware Vulkan; bounded all-k kernel parity, not full-size plotting"]
fn vulkan_bounded_k18_through_k32_kernels_match_cpu() {
    use dg_xch_pos2::{
        compute::{CpuHasher, Work, config},
        core::ProofCore,
        device,
    };

    let ordinal = device_ordinal();
    let cancelled = AtomicBool::new(false);
    let limits = PlotLimits::default();
    for plot_size in (18..=32).step_by(2) {
        let section_bits = if plot_size < 28 { 2 } else { plot_size - 26 };
        let maximum_strength = plot_size - section_bits - 1;
        let maximum_value = u32::MAX >> (32 - plot_size);
        let values = [0, 1, 65_537, maximum_value - 1, maximum_value];
        for strength in [2, 9, maximum_strength] {
            for testnet in [false, true] {
                let params =
                    ProofParams::new([0xa5; 32].into(), plot_size, strength, testnet).unwrap();
                let core = ProofCore::new(params.clone()).unwrap();
                let configuration = config(&params);
                let mut cpu = CpuHasher::new(&params);
                let mut gpu = vulkan::Hasher::for_params(&params, ordinal).unwrap();
                let generated_inputs =
                    values.map(|value| [value ^ if testnet { 0xA3B1C4D7 } else { 0 }, 0, 0, 0]);
                let generated_hashes = Work::new(limits, &cancelled)
                    .hash(&mut gpu, &generated_inputs, 16)
                    .unwrap();
                assert_eq!(
                    generated_hashes,
                    Work::new(limits, &cancelled)
                        .hash(&mut cpu, &generated_inputs, 16)
                        .unwrap(),
                    "generation k={plot_size} strength={strength} testnet={testnet}"
                );
                for (value, lanes) in values.into_iter().zip(generated_hashes) {
                    let entry = device::generate_from_hash(configuration, value, lanes);
                    assert_eq!(entry.info, core.hashing.g(value));
                    assert_eq!(entry, device::generate(configuration, value));
                }
                for table in 1..=3 {
                    if table == 1 && strength == maximum_strength {
                        continue;
                    }
                    let rounds = if table == 1 { 16 << (strength - 2) } else { 16 };
                    let left = device::Record {
                        meta: if table == 1 {
                            u64::from(maximum_value)
                        } else {
                            (u64::from(maximum_value) << plot_size) | 65_537
                        },
                        info: maximum_value,
                        x_bits: maximum_value,
                        xs: [maximum_value; 8],
                        valid: 1,
                        ..Default::default()
                    };
                    let right = device::Record {
                        meta: if table == 1 {
                            u64::from(maximum_value - 1)
                        } else {
                            (u64::from(maximum_value - 1) << plot_size) | u64::from(maximum_value)
                        },
                        info: maximum_value - 1,
                        x_bits: maximum_value - 1,
                        xs: [maximum_value - 1; 8],
                        valid: 1,
                        ..Default::default()
                    };
                    let highest_key = (params.num_match_keys(table as usize) - 1) as u32;
                    let inputs = [
                        [table, 0, left.meta as u32, (left.meta >> 32) as u32],
                        [
                            table,
                            highest_key,
                            left.meta as u32,
                            (left.meta >> 32) as u32,
                        ],
                        [
                            left.meta as u32,
                            (left.meta >> 32) as u32,
                            right.meta as u32,
                            (right.meta >> 32) as u32,
                        ],
                    ];
                    let hashes = Work::new(limits, &cancelled)
                        .hash(&mut gpu, &inputs, rounds)
                        .unwrap();
                    assert_eq!(
                        hashes,
                        Work::new(limits, &cancelled)
                            .hash(&mut cpu, &inputs, rounds)
                            .unwrap(),
                        "matching k={plot_size} strength={strength} table={table} testnet={testnet}"
                    );
                    for (key, lanes) in [0, highest_key].into_iter().zip(&hashes) {
                        let target =
                            device::target_from_hash(configuration, table, left, key, lanes[0]);
                        assert_eq!(target, device::target(configuration, table, left, key));
                        assert!(core.validate_match_info_pairing(
                            table as usize,
                            left.meta,
                            left.info,
                            target
                        ));
                    }
                    assert_eq!(
                        device::pair_from_hash(configuration, table, left, right, hashes[2]),
                        device::pair(configuration, table, left, right)
                    );
                }
                eprintln!(
                    "bounded Vulkan kernels passed: k={plot_size} strength={strength} testnet={testnet}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires hardware Vulkan; compact plotting and fragment solving"]
fn vulkan_compact_plotting_and_fragment_proving_match_cpu() {
    use dg_xch_pos2::{
        chainer::SearchLimits,
        compact::CompactPlot,
        compute::{CpuHasher, Work},
        solver,
        validator::ProofValidator,
    };
    let ordinal = device_ordinal();
    let cancelled = AtomicBool::new(false);
    for (testnet, strength) in [(false, 2), (true, 3)] {
        let params = ProofParams::new([42; 32].into(), 18, strength, testnet).unwrap();
        let limits = PlotLimits::default();
        let cpu = NativePlot::build(params.clone(), limits, &cancelled).unwrap();
        let mut engine = vulkan::Hasher::for_params(&params, ordinal).unwrap();
        let gpu = CompactPlot::build_with_engine(params.clone(), limits, &cancelled, &mut engine)
            .unwrap();
        assert_eq!(cpu.table_counts, gpu.table_counts);
        assert!(
            cpu.witnesses()
                .iter()
                .map(|witness| witness.fragment)
                .eq(gpu.fragments().iter().copied())
        );
        let mut found = false;
        for attempt in 0u16..256 {
            let mut challenge = [0; 32];
            challenge[..2].copy_from_slice(&attempt.to_le_bytes());
            let challenge = challenge.into();
            let chains = cpu
                .qualities(
                    challenge,
                    SearchLimits {
                        max_hashes: 10_000_000,
                        max_results: 1024,
                    },
                    &cancelled,
                )
                .unwrap();
            if let Some(chain) = chains.first() {
                let proof = solver::solve_with_engine(
                    &params,
                    chain,
                    challenge,
                    limits,
                    &cancelled,
                    &mut engine,
                )
                .unwrap();
                assert_eq!(
                    ProofValidator::new(params.clone())
                        .unwrap()
                        .validate_packed_proof(&proof, challenge),
                    Some(chain.fragments)
                );
                found = true;
                break;
            }
        }
        assert!(found);
        let input = [[1, 2, 3, 4]; 17];
        let expected = Work::new(limits, &cancelled)
            .hash(&mut CpuHasher::new(&params), &input, 2048)
            .unwrap();
        let actual = Work::new(limits, &cancelled)
            .hash(&mut engine, &input, 2048)
            .unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
#[ignore = "requires a real Vulkan GPU; run in release mode after ordinary checks"]
fn vulkan_reconstruction_matches_cpu_plot_byte_for_byte() {
    let ordinal = device_ordinal();
    let cancelled = AtomicBool::new(false);
    let limits = PlotLimits::default();
    let directory = tempfile::tempdir().unwrap();
    let farmer_key = SecretKey::key_gen_v3(&[7; 32], &[])
        .unwrap()
        .sk_to_pk()
        .to_bytes();
    for testnet in [false, true] {
        let request = PlotRequest {
            farmer_public_key: farmer_key,
            pool: PoolBinding::Contract([8; 32]),
            k: 18,
            strength: 2,
            index: 256,
            meta_group: 3,
            testnet,
        };
        let cpu_path = directory.path().join(format!("cpu-{testnet}.plot"));
        let cpu_info =
            create_with_engine(&request, &cpu_path, limits, &cancelled, NativePlot::build).unwrap();
        let reconstructed = ReconstructedPlot::open_with_engine(
            &cpu_path,
            testnet,
            limits,
            &cancelled,
            |params, limits, cancelled| vulkan::build(params, limits, cancelled, ordinal),
        )
        .unwrap();
        assert_eq!(reconstructed.info, cpu_info);
    }
}

#[test]
#[ignore = "requires a real Vulkan GPU; run in release mode after ordinary checks"]
fn vulkan_witnesses_match_cpu_in_both_domains() {
    let ordinal = device_ordinal();
    let cancelled = AtomicBool::new(false);
    for (testnet, strength) in [(false, 2), (true, 3)] {
        let params = ProofParams::new([42; 32].into(), 18, strength, testnet).unwrap();
        let cpu = NativePlot::build(params.clone(), PlotLimits::default(), &cancelled).unwrap();
        let gpu = vulkan::build(params, PlotLimits::default(), &cancelled, ordinal).unwrap();
        assert_eq!(cpu.table_counts, gpu.table_counts);
        assert_eq!(cpu.witnesses(), gpu.witnesses());
    }
}
