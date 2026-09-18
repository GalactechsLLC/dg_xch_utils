use super::*;
use crate::consensus::constants::MAINNET;

#[test]
fn overrides_apply_and_leave_the_rest_untouched() {
    let o = ConsensusOverrides {
        discriminant_size_bits: Some(BigInt::from(16)),
        difficulty_starting: Some(30),
        epoch_blocks: Some(768),
        min_plot_size: Some(18),
        ..Default::default()
    };
    let c = apply_overrides(MAINNET, &o);
    assert_eq!(c.discriminant_size_bits, 16);
    assert_eq!(c.difficulty_starting, 30);
    assert_eq!(c.epoch_blocks, 768);
    assert_eq!(c.min_plot_size, 18);
    // An unset field keeps the base value.
    assert_eq!(c.sub_slot_iters_starting, MAINNET.sub_slot_iters_starting);
    assert_eq!(c.genesis_challenge, MAINNET.genesis_challenge);
}

#[test]
fn no_overrides_is_the_identity() {
    let c = apply_overrides(MAINNET, &ConsensusOverrides::default());
    assert_eq!(c.discriminant_size_bits, MAINNET.discriminant_size_bits);
    assert_eq!(c.epoch_blocks, MAINNET.epoch_blocks);
}
