use dg_xch_core::blockchain::class_group_element::ClassgroupElement;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_timelord::worker::{ProofRequest, ProofResult, RegularProofRequest};
use dg_xch_vdf::proof::verify_vdf_serial;
use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn regular_worker_subprocess_returns_verified_full_discriminant_proof() {
    let request = RegularProofRequest {
        request: ProofRequest {
            generation: 42,
            challenge: Bytes32::from([41; 32]),
            input: ClassgroupElement::get_default_element(),
            iterations: 9,
            discriminant_bits: 1024,
        },
        memory_bytes: 128 * 1024,
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_dg_xch_timelord"))
        .arg("regular-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let result: ProofResult = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.generation, request.request.generation);
    assert_eq!(result.info.challenge, request.request.challenge);
    assert_eq!(result.info.number_of_iterations, request.request.iterations);
    assert!(!result.proof.normalized_to_identity);
    let mut proof = result.info.output.data.to_vec();
    proof.extend_from_slice(result.proof.witness.as_slice());
    assert!(verify_vdf_serial(
        request.request.challenge.as_ref(),
        request.request.input.data.as_ref(),
        &proof,
        request.request.discriminant_bits,
        request.request.iterations,
        0,
    ));
}
