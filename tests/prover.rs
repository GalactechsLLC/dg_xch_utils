use dg_xch_utils::clvm::utils::hash_256;
use dg_xch_utils::proof_of_space::prover::DiskProver;
use dg_xch_utils::proof_of_space::verifier::validate_proof;
use dg_xch_utils::types::blockchain::sized_bytes::Bytes32;
use std::path::Path;

//
// uint32_t iterations: u32,
// uint8_t k: u8,
// uint8_t* plot_id: ,
// uint32_t num_proofs

fn test_proof_of_space(filename: &str, iterations: u32, num_proofs: u32) {
    let prover = DiskProver::new(Path::new(filename)).unwrap();
    let k = prover.header.k;
    let plot_id = prover.header.id;
    let mut proof_data;
    let mut success = 0;
    // Tries an edge case challenge with many 1s in the front, and ensures there is no segfault
    prover
        .get_qualities_for_challenge(&Bytes32::from(
            "fffffa2b647d4651c500076d7df4c6f352936cf293bd79c591a7b08e43d6adfb",
        ))
        .unwrap();
    let mut invalid = 0;
    for i in 0u32..iterations {
        let hash_input = i.to_be_bytes();
        let hash = hash_256(hash_input.as_slice());
        let challenge = Bytes32::from(hash);
        let qualities = prover.get_qualities_for_challenge(&challenge).unwrap();
        for index in 0..qualities.len() {
            if let Ok(proof) = prover.get_full_proof(&challenge, index, true) {
                proof_data = proof.to_bytes();
                let quality = validate_proof(&plot_id, k, &challenge.bytes, &proof_data).unwrap();
                if quality.get_size() != 256 {
                    invalid += 1;
                    continue;
                }
                assert!(quality == qualities[index]);
                success += 1;
                // Tests invalid proof
                proof_data[0] = ((proof_data[0] as u16 + 1) % 256) as u8;
                let quality_2 = validate_proof(&plot_id, k, &challenge.bytes, &proof_data).unwrap();
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
    assert_eq!(success, num_proofs);
    assert!(success as f32 > 0.5f32 * iterations as f32);
    assert!(success < (1.5f32 * iterations as f32) as u32);
}

#[test]
fn pos_test() {
    let path_str = "/mnt/1ee4f490-0fd0-4fb4-9dd2-9df897b628a7/chia_plots/plot-k25-2022-12-09-17-18-0afc8becaf6a6c761e18c682b1a52e0da0cefa50e157ee1963ee983d6c6738d9.plot";
    let iterations = 100;
    test_proof_of_space(path_str, iterations, 86);
}

#[test]
fn test_prover() {
    let path_str = "/mnt/1ee4f490-0fd0-4fb4-9dd2-9df897b628a7/chia_plots/plot-k25-2022-12-09-17-18-0afc8becaf6a6c761e18c682b1a52e0da0cefa50e157ee1963ee983d6c6738d9.plot";
    let path = Path::new(path_str);
    let prover = DiskProver::new(path).unwrap();
    let challenge = Bytes32::from(vec![24; 32]);
    let res = prover.get_qualities_for_challenge(&challenge).unwrap();
    println!("Found {} qualities for {}", res.len(), &challenge.as_str);
}
