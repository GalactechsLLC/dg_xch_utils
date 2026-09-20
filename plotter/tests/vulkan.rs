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
