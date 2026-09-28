use blst::min_pk::SecretKey;
use dg_xch_plotter::{PlotRequest, PoolBinding, create, inspect};
use dg_xch_pos2::{
    chainer::SearchLimits,
    params::ProofParams,
    plotting::{NativePlot, PlotLimits},
    validator::ProofValidator,
};
use std::sync::atomic::AtomicBool;

fn request(portable: bool, testnet: bool) -> PlotRequest {
    let farmer = SecretKey::key_gen_v3(&[7; 32], &[])
        .unwrap()
        .sk_to_pk()
        .to_bytes();
    PlotRequest {
        farmer_public_key: farmer,
        pool: if portable {
            PoolBinding::Contract([8; 32])
        } else {
            PoolBinding::PublicKey(farmer)
        },
        k: 18,
        strength: 2,
        index: 256,
        meta_group: 3,
        testnet,
    }
}

#[test]
fn reconstructed_plot_rejects_payload_corruption_and_cancellation() {
    use dg_xch_plotter::proving::ReconstructedPlot;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reconstruct.plot");
    let cancelled = AtomicBool::new(false);
    let limits = PlotLimits::default();
    let info = create(&request(true, false), &path, limits, &cancelled).unwrap();
    let plot = ReconstructedPlot::open(&path, false, limits, &cancelled).unwrap();
    assert_eq!(plot.info, info);
    assert!(ReconstructedPlot::open(&path, false, limits, &AtomicBool::new(true)).is_err());
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(inspect(&path).is_ok());
    assert!(ReconstructedPlot::open(&path, false, limits, &cancelled).is_err());
}

#[test]
fn malformed_headers_fail_without_panics() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.plot");
    for length in 0..=256 {
        let mut bytes = vec![0; length];
        if length >= 43 {
            bytes[..4].copy_from_slice(b"pos2");
            bytes[4] = 1;
            bytes[37] = 18;
            bytes[38] = 2;
            bytes[42] = 255;
        }
        std::fs::write(&path, bytes).unwrap();
        assert!(inspect(&path).is_err());
    }
}

#[test]
fn refuses_overwrite_and_invalid_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing.plot");
    std::fs::write(&path, b"keep").unwrap();
    let cancelled = AtomicBool::new(false);
    assert!(
        create(
            &request(false, false),
            &path,
            PlotLimits::default(),
            &cancelled
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    let mut invalid = request(false, false);
    invalid.farmer_public_key = [0; 48];
    assert!(
        create(
            &invalid,
            &directory.path().join("invalid.plot"),
            PlotLimits::default(),
            &cancelled
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    let cancelled_path = directory.path().join("cancelled.plot");
    let error = create(
        &request(false, false),
        &cancelled_path,
        PlotLimits::default(),
        &AtomicBool::new(true),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!cancelled_path.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn native_pipeline_obeys_resource_limits_and_cancellation() {
    let params = ProofParams::new([1; 32].into(), 18, 2, false).unwrap();
    assert!(
        NativePlot::build(
            params.clone(),
            PlotLimits {
                memory_bytes: 1,
                ..PlotLimits::default()
            },
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(
        NativePlot::build(
            params.clone(),
            PlotLimits {
                max_entries: 1,
                ..PlotLimits::default()
            },
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(
        NativePlot::build(
            params.clone(),
            PlotLimits {
                max_work: 1,
                ..PlotLimits::default()
            },
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(
        NativePlot::build(params, PlotLimits::default(), &AtomicBool::new(true))
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
#[ignore = "requires external pinned Chia reference writer in DGX_POS2_REFERENCE_BIN"]
fn plot_files_match_chia_byte_for_byte_at_k18() {
    let reference = std::env::var_os("DGX_POS2_REFERENCE_BIN")
        .expect("set DGX_POS2_REFERENCE_BIN to compiled reference_writer.cpp");
    for portable in [false, true] {
        for testnet in [false, true] {
            for strength in [2, 3] {
                let directory = tempfile::tempdir().unwrap();
                let native = directory.path().join("native.plot");
                let expected = directory.path().join("chia.plot");
                let mut request = request(portable, testnet);
                request.strength = strength;
                create(
                    &request,
                    &native,
                    PlotLimits::default(),
                    &AtomicBool::new(false),
                )
                .unwrap();
                let status = std::process::Command::new(&reference)
                    .arg(&native)
                    .arg(&expected)
                    .arg(testnet.to_string())
                    .status()
                    .unwrap();
                assert!(status.success(), "reference writer failed");
                let actual = std::fs::read(&native).unwrap();
                let expected = std::fs::read(&expected).unwrap();
                let mismatch = actual
                    .iter()
                    .zip(&expected)
                    .position(|(actual, expected)| actual != expected);
                assert!(
                    actual == expected,
                    "byte mismatch: portable={portable} testnet={testnet} strength={strength} offset={mismatch:?} native_len={} chia_len={}",
                    actual.len(),
                    expected.len()
                );
                eprintln!(
                    "exact match: k18 s{strength} portable={portable} testnet={testnet}, {} bytes",
                    actual.len()
                );
            }
        }
    }
}

#[test]
#[ignore = "constructs two full native k18 plots and proofs"]
fn native_tables_generate_valid_proofs_in_both_network_modes() {
    let cancelled = AtomicBool::new(false);
    for testnet in [false, true] {
        let params = ProofParams::new([9; 32].into(), 18, 2, testnet).unwrap();
        let plot = NativePlot::build(params.clone(), PlotLimits::default(), &cancelled).unwrap();
        let validator = ProofValidator::new(params).unwrap();
        assert!(plot.table_counts.iter().all(|count| *count > 100_000));
        assert!(
            plot.witnesses()
                .windows(2)
                .all(|pair| pair[0].fragment <= pair[1].fragment)
        );
        for witness in plot.witnesses().iter().step_by(113) {
            assert!(validator.validate_table_3_pairs(&witness.xs).is_some());
            assert_eq!(
                validator.core().fragment_codec.encode(&witness.xs),
                witness.fragment
            );
        }
        let mut found = false;
        for attempt in 0u16..256 {
            let mut challenge = [0; 32];
            challenge[..2].copy_from_slice(&attempt.to_le_bytes());
            let challenge = challenge.into();
            let chains = plot
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
                let proof = plot.prove(chain, challenge).unwrap();
                assert_eq!(
                    validator.validate_packed_proof(&proof, challenge),
                    Some(chain.fragments)
                );
                found = true;
                break;
            }
        }
        assert!(found);
    }
}

#[test]
#[ignore = "writes two real plots using the native Rust pipeline"]
fn native_plot_files_bind_memos_and_reject_corruption() {
    for portable in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.plot");
        let info = create(
            &request(portable, portable),
            &path,
            PlotLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(info, inspect(&path).unwrap());
        assert_eq!(info.portable, portable);
        assert_eq!(info.index, 256);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[5] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(inspect(&path).is_err());
        bytes[5] ^= 1;
        bytes.pop();
        std::fs::write(&path, bytes).unwrap();
        assert!(inspect(&path).is_err());
    }
}
