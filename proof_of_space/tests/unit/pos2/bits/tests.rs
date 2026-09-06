use super::*;

#[test]
fn packing_round_trips() {
    for k in [8u8, 18, 24, 28, 32] {
        let mask = if k == 32 { u32::MAX } else { (1u32 << k) - 1 };
        let xs: Vec<u32> = (0..128u32)
            .map(|i| i.wrapping_mul(2_654_435_761) & mask)
            .collect();
        let packed = compact_bits(&xs, k);
        assert_eq!(packed.len(), (128 * usize::from(k)).div_ceil(8), "k{k}");
        assert_eq!(expand_bits(&packed, k).expect("expands"), xs, "k{k}");
    }
}

#[test]
fn a_k18_proof_is_288_bytes() {
    assert_eq!(compact_bits(&[0u32; 128], 18).len(), 288);
}
