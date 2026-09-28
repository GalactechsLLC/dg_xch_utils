use super::*;
use dg_xch_core::consensus::constants::{ConsensusConstants, SIMULATOR};
use dg_xch_core::consensus::overrides::{ConsensusOverrides, apply_overrides};
use dg_xch_vdf::validate_vdf_info;
use num_bigint::BigInt;

fn constants_at(discriminant_size_bits: i64) -> ConsensusConstants {
    apply_overrides(
        SIMULATOR,
        &ConsensusOverrides {
            discriminant_size_bits: Some(BigInt::from(discriminant_size_bits)),
            ..Default::default()
        },
    )
}

#[test]
fn a_tiny_discriminant_proof_validates_through_the_real_verifier() {
    // Shrinking the discriminant keeps the same prover and verifier path.
    for bits in [16i64, 32, 64, 512] {
        let constants = constants_at(bits);
        let challenge = Bytes32::from([7u8; 32]);
        let input = ClassgroupElement::get_default_element();
        let (info, proof) = prove_vdf(challenge, &input, 64, constants.discriminant_size_bits)
            .unwrap_or_else(|e| panic!("prove at {bits} bits: {e}"));
        assert_eq!(info.number_of_iterations, 64);
        assert!(
            validate_vdf_info(&constants, &input, &info, &proof, None),
            "a {bits} bit proof was rejected by the real verifier"
        );
    }
}

#[test]
fn a_proof_is_rejected_under_a_different_iteration_count() {
    let constants = constants_at(16);
    let challenge = Bytes32::from([9u8; 32]);
    let input = ClassgroupElement::get_default_element();
    let (mut info, proof) = prove_vdf(challenge, &input, 32, 16).expect("prove");
    info.number_of_iterations = 33;
    assert!(!validate_vdf_info(&constants, &input, &info, &proof, None));
}

#[test]
fn a_tampered_witness_is_rejected() {
    let constants = constants_at(16);
    let challenge = Bytes32::from([5u8; 32]);
    let input = ClassgroupElement::get_default_element();
    let (info, mut proof) = prove_vdf(challenge, &input, 32, 16).expect("prove");
    let mut witness = proof.witness.as_slice().to_vec();
    witness[0] ^= 0xFF;
    proof.witness = UnsizedBytes::new(witness);
    assert!(!validate_vdf_info(&constants, &input, &info, &proof, None));
}

#[test]
fn proving_is_deterministic() {
    let challenge = Bytes32::from([1u8; 32]);
    let input = ClassgroupElement::get_default_element();
    let a = prove_vdf(challenge, &input, 48, 16).expect("prove");
    let b = prove_vdf(challenge, &input, 48, 16).expect("prove");
    assert_eq!(a.0.output, b.0.output);
    assert_eq!(a.1.witness.as_slice(), b.1.witness.as_slice());
}

#[test]
fn chaining_a_proof_onto_the_previous_output_validates() {
    // Each infusion continues from the last output rather than restarting from the identity.
    let constants = constants_at(16);
    let challenge = Bytes32::from([3u8; 32]);
    let first_input = ClassgroupElement::get_default_element();
    let (first, first_proof) = prove_vdf(challenge, &first_input, 32, 16).expect("prove");
    assert!(validate_vdf_info(
        &constants,
        &first_input,
        &first,
        &first_proof,
        None
    ));

    let (second, second_proof) = prove_vdf(challenge, &first.output, 32, 16).expect("prove");
    assert!(validate_vdf_info(
        &constants,
        &first.output,
        &second,
        &second_proof,
        None
    ));
    assert_ne!(first.output, second.output);
}
