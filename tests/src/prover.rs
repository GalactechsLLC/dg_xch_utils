use dg_xch_core::blockchain::sized_bytes::{Bytes32, SizedBytes};
use dg_xch_pos::prover::DiskProver;
use dg_xch_pos::verifier::validate_proof;
use dg_xch_serialize::{hash_256, ChiaSerialize};
use std::io::Error;
use std::path::Path;

pub fn test_proof_of_space(filename: &str, iterations: u32) -> Result<u32, Error> {
    let prover = DiskProver::new(Path::new(filename)).unwrap();
    let k = prover.header.k;
    let plot_id = &prover.header.id;
    let mut proof_data;
    let mut success = 0;
    // Tries an edge case challenge with many 1s in the front, and ensures there is no segfault
    prover.get_qualities_for_challenge(&Bytes32::from(
        "fffffa2b647d4651c500076d7df4c6f352936cf293bd79c591a7b08e43d6adfb",
    ))?;
    let mut invalid = 0;
    for i in 0u32..iterations {
        let hash_input = i.to_be_bytes();
        let hash = hash_256(hash_input.as_slice());
        let challenge = Bytes32::new(&hash);
        let qualities = prover.get_qualities_for_challenge(&challenge)?;
        for (index, vec) in qualities.iter().enumerate() {
            if let Ok(proof) = prover.get_full_proof(&challenge, index, true) {
                proof_data = proof.to_bytes();
                let quality =
                    validate_proof(plot_id.to_sized_bytes(), k, &challenge.bytes, &proof_data)
                        .unwrap();
                if quality.get_size() != 256 {
                    invalid += 1;
                    continue;
                }
                assert!(quality == *vec);
                success += 1;
                // Tests invalid proof
                proof_data[0] = ((proof_data[0] as u16 + 1) % 256) as u8;
                let quality_2 =
                    validate_proof(plot_id.to_sized_bytes(), k, &challenge.bytes, &proof_data)
                        .unwrap();
                assert_eq!(quality_2.get_size(), 0);
            } else {
                invalid += 1;
            }
        }
    }
    println!(
        "Invalid: {invalid}, Success: {success} / {iterations} {}%",
        (100f64 * (success as f64 / iterations as f64))
    );
    Ok(success)
}

#[test]
fn pos_test() {
    let path_str = "/mnt/1ee4f490-0fd0-4fb4-9dd2-9df897b628a7/chia_plots/plot-k25-2022-12-09-17-18-0afc8becaf6a6c761e18c682b1a52e0da0cefa50e157ee1963ee983d6c6738d9.plot";
    let iterations = 100;
    let success = test_proof_of_space(path_str, iterations).unwrap();
    assert_eq!(success, 86);
    assert!(success as f32 > 0.5f32 * iterations as f32);
    assert!(success < (1.5f32 * iterations as f32) as u32);
}

#[test]
fn test_prover() {
    let path_str = "/mnt/1ee4f490-0fd0-4fb4-9dd2-9df897b628a7/chia_plots/plot-k25-2022-12-09-17-18-0afc8becaf6a6c761e18c682b1a52e0da0cefa50e157ee1963ee983d6c6738d9.plot";
    let path = Path::new(path_str);
    let prover = DiskProver::new(path).unwrap();
    let challenge = Bytes32::from([24; 32]);
    let res = prover.get_qualities_for_challenge(&challenge).unwrap();
    println!("Found {} qualities for {}", res.len(), &challenge);
}
