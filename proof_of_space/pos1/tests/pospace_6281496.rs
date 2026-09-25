// Regression: live-mainnet block 6,281,496 (a k41 winning proof) was rejected with
// chiapos "Values Not in Same Group" (surfaced as INVALID_POSPACE), walling k>32 farmers. Root cause:
// `BitReader::from_bytes_be_offset` under-counted the 64-bit field span for k>32 metadata
// widths, overshooting the final-field shift (panic in debug / wrapping garbage in release ->
// corrupt metadata -> a false fx_match failure). k32 never crossed the boundary; k41 does.
//
// Fixture layout (primitives extracted from the real block, no wire/zstd deps):
//   k(1) || plot_id(32) || challenge(32) || proof(328)   = 393 bytes
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_pos1::verifier::validate_proof;

const FIXTURE: &[u8] = include_bytes!("fixtures/pospace_6281496.bin");

#[test]
fn k41_block_6281496_validates() {
    let k = FIXTURE[0];
    let plot_id: [u8; 32] = FIXTURE[1..33].try_into().unwrap();
    let challenge = &FIXTURE[33..65];
    let proof = &FIXTURE[65..];
    assert_eq!(k, 41, "fixture is the k41 proof");
    assert_eq!(proof.len(), 328, "64 * 41 bits = 328 bytes");

    // The exact call get_quality_string makes. Pre-fix this panicked (debug) / mis-decoded
    // (release); post-fix it returns the real non-zero quality string.
    let quality = validate_proof(&plot_id, k, proof, challenge)
        .expect("k41 pospace must decode without error");
    assert_ne!(
        quality,
        Bytes32::default(),
        "f7 must match the challenge -> non-zero quality (proof is valid)"
    );
    // Lock the exact quality so any future decode regression is caught byte-for-byte.
    assert_eq!(
        quality.to_string(),
        "0x515fac0a1c958a26856dfeae617220c7f8ce6e5552fe68fd59b1fb6c3c2e2002",
    );
}
#[test]
fn swapped_proof_subtrees_are_rejected_but_explicit_reordering_still_works() {
    let size = FIXTURE[0];
    let plot_id: [u8; 32] = FIXTURE[1..33].try_into().unwrap();
    let challenge = &FIXTURE[33..65];
    let original = dg_xch_pos1::verifier::uncompress_proof(&FIXTURE[65..], usize::from(size));
    for width in [1, 2, 4, 8, 16, 32] {
        let mut values = original.clone();
        values[..2 * width].rotate_left(width);
        let proof = dg_xch_pos1::plots::plot_writer::proof_bytes(size, &values);
        let result = validate_proof(&plot_id, size, &proof, challenge);
        assert!(result.is_err() || result.unwrap() == Bytes32::default());
        let mut outputs = [0; 64];
        let mut metadata = Vec::new();
        dg_xch_pos1::plots::fx_generator::get_proof_f1_and_meta(
            u32::from(size),
            &plot_id,
            &values,
            &mut outputs,
            &mut metadata,
        )
        .unwrap();
        dg_xch_pos1::plots::fx_generator::forward_prop_f1_to_f7(
            Some(&mut values),
            &mut outputs,
            &mut metadata,
            u32::from(size),
        )
        .unwrap();
        assert_eq!(values, original);
    }
}

#[test]
fn malformed_proof_shapes_return_errors_without_panicking() {
    let size = FIXTURE[0];
    let plot_id: [u8; 32] = FIXTURE[1..33].try_into().unwrap();
    let challenge = &FIXTURE[33..65];
    let proof = &FIXTURE[65..];
    let mut extended = proof.to_vec();
    extended.push(0);
    for malformed in [&proof[..0], &proof[..proof.len() - 1], extended.as_slice()] {
        assert!(validate_proof(&plot_id, size, malformed, challenge).is_err());
    }
    for malformed in [&challenge[..0], &challenge[..31], &[0; 33][..]] {
        assert!(validate_proof(&plot_id, size, proof, malformed).is_err());
    }
    for invalid_size in [0, 1, 5, 59, 64, 255] {
        assert!(validate_proof(&plot_id, invalid_size, proof, challenge).is_err());
    }
}
