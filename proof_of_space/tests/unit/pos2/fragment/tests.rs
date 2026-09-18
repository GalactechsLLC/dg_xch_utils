use super::*;

fn plot_id() -> Bytes32 {
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i * 11 + 5) as u8;
    }
    Bytes32::from(bytes)
}

fn sample_xs(k: u32) -> [u32; 8] {
    let mask = (1u32 << k) - 1;
    std::array::from_fn(|i| ((i as u32) * 7919 + 13) & mask)
}

#[test]
fn encoding_matches_the_reference_vectors() {
    assert_eq!(
        ProofFragmentCodec::new(plot_id(), 28)
            .expect("codec")
            .encode(&sample_xs(28)),
        27_747_203_251_943_674
    );
    assert_eq!(
        ProofFragmentCodec::new(plot_id(), 30)
            .expect("codec")
            .encode(&sample_xs(30)),
        75_867_245_716_127_150
    );
}

#[test]
fn a_fragment_recovers_the_surviving_halves() {
    for k in [28u32, 30] {
        let codec = ProofFragmentCodec::new(plot_id(), k).expect("codec");
        let xs = sample_xs(k);
        let fragment = codec.encode(&xs);
        let half = k / 2;
        assert_eq!(
            codec.x_bits(fragment),
            [xs[0] >> half, xs[2] >> half, xs[4] >> half, xs[6] >> half],
            "k{k}"
        );
        assert!(codec.validates(fragment, &xs), "k{k}");
    }
}

#[test]
fn the_dropped_x_values_do_not_change_the_fragment() {
    // x2, x4, x6 and x8 contribute nothing, and neither do the low halves of the others.
    let codec = ProofFragmentCodec::new(plot_id(), 28).expect("codec");
    let mut xs = sample_xs(28);
    let before = codec.encode(&xs);
    xs[1] ^= 0xFFF;
    xs[3] ^= 0xFFF;
    xs[0] ^= 1;
    assert_eq!(codec.encode(&xs), before);
}

#[test]
fn a_changed_surviving_half_changes_the_fragment() {
    let codec = ProofFragmentCodec::new(plot_id(), 28).expect("codec");
    let mut xs = sample_xs(28);
    let before = codec.encode(&xs);
    xs[4] ^= 1 << 20;
    assert_ne!(codec.encode(&xs), before);
    assert!(!codec.validates(before, &xs));
}
