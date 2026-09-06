use super::*;
use crate::discriminant::create_discriminant_int;
use crate::form::{B_BYTES, get_b};

#[test]
fn memoized_verify_vdf_is_identical_to_uncached() {
    let challenge = hex::decode("9f1fbdf9b1a0b6912cd5e2a4b40a2ffb1810b513994b1dd6c4e6df9c30de5f6e")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();
    let proof = prove_with_discriminant(&discriminant, &x, 100).unwrap();

    // Miss, then hit — both must equal the uncached result.
    assert!(verify_vdf(&challenge, &x_s, &proof, 1024, 100, 0));
    assert!(verify_vdf(&challenge, &x_s, &proof, 1024, 100, 0));
    assert!(verify_vdf_uncached(
        &challenge, &x_s, &proof, 1024, 100, 0, true
    ));

    // An invalid proof of the correct length: a distinct key, recomputed, false — on the
    // miss and the hit path (i.e. `false` results are cached too, and a bad proof can never
    // inherit the valid proof's cached `true`).
    let bad = vec![0xFFu8; proof.len()];
    assert!(!verify_vdf(&challenge, &x_s, &bad, 1024, 100, 0));
    assert!(!verify_vdf(&challenge, &x_s, &bad, 1024, 100, 0));
    assert!(!verify_vdf_uncached(
        &challenge, &x_s, &bad, 1024, 100, 0, true
    ));

    // A changed iteration count is a distinct key (length-prefixed fields, fixed-width
    // trailer — no concatenation ambiguity), so the cached `true` above cannot leak here.
    assert!(!verify_vdf(&challenge, &x_s, &proof, 1024, 101, 0));
}

/// The serial (no inner threads) verification path must be result-identical to the parallel
/// path, below the memo — valid proof, valid recursive proof, and an invalid proof alike.
#[test]
fn serial_verification_is_result_identical_to_parallel() {
    let challenge = hex::decode("1f0c94d5d1f5ea25be3b04e04d17806bcc9a0dbcdcc16346eb388937b5981c37")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();
    let proof = prove_with_discriminant(&discriminant, &x, 73).unwrap();

    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 73, 0, true).is_ok());
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 73, 0, false).is_ok());
    // Wrong iteration count: both paths must reject.
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 74, 0, true).is_err());
    assert!(check_n_wesolowski_impl(&discriminant, &x_s, &proof, 74, 0, false).is_err());
    // The public serial entry point (through the memo) agrees as well.
    assert!(verify_vdf_serial(&challenge, &x_s, &proof, 1024, 73, 0));
}

/// The memo stays bounded past capacity.
#[test]
fn verify_memo_stays_bounded() {
    let challenge = hex::decode("8be26af52b34a1a7c47a35c7f0c1add793d5b6e2b0e56e6e970cbd6bd4e17e2a")
        .expect("challenge hex is valid");
    let x_s = [0u8; 100];
    for i in 0..(VERIFY_MEMO_CAPACITY as u64 + 100) {
        let _ = verify_vdf(&challenge, &x_s, &[0u8; 8], 1024, i, 0);
    }
    assert!(verify_memo_len() <= VERIFY_MEMO_CAPACITY);
}

#[test]
fn recursive_proof_verifies_segment_before_final_witness() {
    let challenge = hex::decode("ccd5bb71183532bff220ba46c268991a3ff07eb358e8255a65c30a2dce0e5fbb")
        .expect("challenge hex is valid");
    let discriminant = create_discriminant_int(&challenge, 1024).unwrap();
    let mut x_s = [0u8; 100];
    x_s[0] = 0x08;
    let x = Form::deserialize(&discriminant, &x_s).unwrap();

    let segment_iterations = 24;
    let final_iterations = 31;
    let segment_proof = prove_with_discriminant(&discriminant, &x, segment_iterations).unwrap();
    let segment_y = Form::deserialize(&discriminant, &segment_proof[..FORM_SIZE]).unwrap();
    let segment_witness = &segment_proof[FORM_SIZE..FORM_SIZE * 2];
    let segment_b = get_b(&discriminant, &x, &segment_y).unwrap();

    let mut recursive_proof =
        prove_with_discriminant(&discriminant, &segment_y, final_iterations).unwrap();
    recursive_proof.extend_from_slice(&segment_iterations.to_be_bytes());
    recursive_proof.extend_from_slice(&fixed_be_bytes(&segment_b, B_BYTES));
    recursive_proof.extend_from_slice(segment_witness);

    check_n_wesolowski(
        &discriminant,
        &x_s,
        &recursive_proof,
        segment_iterations + final_iterations,
        1,
    )
    .expect("depth-1 proof should verify");
}

fn fixed_be_bytes(value: &BigInt, size: usize) -> Vec<u8> {
    let (_, bytes) = value.to_bytes_be();
    assert!(bytes.len() <= size);
    let mut out = vec![0u8; size - bytes.len()];
    out.extend_from_slice(&bytes);
    out
}
