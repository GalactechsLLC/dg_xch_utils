use dg_xch_plotter::reader::PlotReader;
use dg_xch_pos2::{chainer::SearchLimits, plotting::PlotLimits, validator::ProofValidator};
use std::path::Path;
use std::sync::atomic::AtomicBool;

#[test]
#[ignore = "requires an existing plot in DGX_POS2_TEST_PLOT; scans its x-space to recover a proof"]
fn existing_plot_recovers_independently_verified_proof() {
    let path = std::env::var_os("DGX_POS2_TEST_PLOT").expect("set DGX_POS2_TEST_PLOT");
    let cancelled = AtomicBool::new(false);
    let mut limits = PlotLimits::default();
    if let Ok(value) = std::env::var("DGX_POS2_TEST_MEMORY_MIB") {
        limits.memory_bytes = value
            .parse::<u64>()
            .unwrap()
            .checked_mul(1024 * 1024)
            .unwrap();
    }
    if let Ok(value) = std::env::var("DGX_POS2_TEST_MAX_ENTRIES") {
        limits.max_entries = value.parse().unwrap();
    }
    if let Ok(value) = std::env::var("DGX_POS2_TEST_MAX_WORK") {
        limits.max_work = value.parse().unwrap();
    }
    let mut plot = PlotReader::open(Path::new(&path), false, limits.memory_bytes).unwrap();
    let validator = ProofValidator::new(plot.params().clone()).unwrap();
    let mut found = false;
    for attempt in 0u16..256 {
        let mut challenge = [0; 32];
        challenge[..2].copy_from_slice(&attempt.to_le_bytes());
        let chains = plot
            .qualities(
                challenge.into(),
                SearchLimits {
                    max_hashes: 10_000_000,
                    max_results: 1024,
                },
                &cancelled,
            )
            .unwrap();
        if let Some(chain) = chains.first() {
            let start = std::time::Instant::now();
            let proof = if let Some(helper) = std::env::var_os("DGX_CUDA_TEST_BIN") {
                let output = std::process::Command::new(helper)
                    .arg("--prove-plot")
                    .arg(&path)
                    .arg("--challenge")
                    .arg(hex::encode(challenge))
                    .arg("--memory-mib")
                    .arg((limits.memory_bytes / 1024 / 1024).to_string())
                    .arg("--max-entries")
                    .arg(limits.max_entries.to_string())
                    .arg("--max-work")
                    .arg(limits.max_work.to_string())
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "CUDA proof recovery failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let output = String::from_utf8(output.stdout).unwrap();
                let proof = output
                    .lines()
                    .find_map(|line| line.split_once(" proof=").map(|(_, proof)| proof))
                    .expect("CUDA returned no proof");
                hex::decode(proof).unwrap()
            } else if let Ok(ordinal) = std::env::var("DGX_VULKAN_TEST_DEVICE") {
                #[cfg(feature = "vulkan")]
                {
                    let mut engine = dg_xch_pos2::vulkan::Hasher::for_params(
                        plot.params(),
                        ordinal.parse().unwrap(),
                    )
                    .unwrap();
                    plot.prove_with_engine(chain, challenge.into(), limits, &cancelled, &mut engine)
                        .unwrap()
                }
                #[cfg(not(feature = "vulkan"))]
                {
                    panic!("rebuild with vulkan to use ordinal {ordinal}");
                }
            } else {
                plot.prove(chain, challenge.into(), limits, &cancelled)
                    .unwrap()
            };
            assert_eq!(
                validator.validate_packed_proof(&proof, challenge.into()),
                Some(chain.fragments)
            );
            if let Some(reference) = std::env::var_os("DGX_POS2_REFERENCE_BIN") {
                assert!(
                    std::process::Command::new(reference)
                        .arg(&path)
                        .arg("verify")
                        .arg("false")
                        .arg(hex::encode(challenge))
                        .arg(hex::encode(&proof))
                        .status()
                        .unwrap()
                        .success(),
                    "Chia rejected the recovered proof or could not read its fragments from the plot"
                );
            }
            eprintln!(
                "k={} strength={} challenge={} proof_seconds={:.3}",
                plot.info.k,
                plot.info.strength,
                hex::encode(challenge),
                start.elapsed().as_secs_f64()
            );
            found = true;
            break;
        }
    }
    assert!(found);
}
