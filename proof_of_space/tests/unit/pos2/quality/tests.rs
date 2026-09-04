use super::*;

#[test]
fn the_commitment_is_strength_then_little_endian_links() {
    let mut fragments = [0u64; NUM_CHAIN_LINKS];
    fragments[0] = 25_078_806_449;
    let out = serialize_quality(&fragments, 2);
    assert_eq!(out.len(), 129);
    assert_eq!(out[0], 2);
    // The fragment, little endian.
    assert_eq!(
        &out[1..9],
        &[0xb1, 0x37, 0xd0, 0xd6, 0x05, 0x00, 0x00, 0x00]
    );
    assert_eq!(&out[9..17], &[0u8; 8]);
}

#[test]
fn the_quality_moves_with_every_link_and_the_strength() {
    let fragments = [7u64; NUM_CHAIN_LINKS];
    let base = quality_hash(&fragments, 2);
    assert_eq!(base, quality_hash(&fragments, 2));
    assert_ne!(base, quality_hash(&fragments, 3));
    let mut changed = fragments;
    changed[NUM_CHAIN_LINKS - 1] += 1;
    assert_ne!(base, quality_hash(&changed, 2));
}
