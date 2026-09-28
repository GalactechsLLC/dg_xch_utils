use super::*;

fn c() -> ConsensusConstants {
    dg_xch_core::consensus::constants::MAINNET
}

#[test]
fn deficit_genesis_is_min_minus_one() {
    assert_eq!(calculate_deficit(&c(), 0, None, false, 0), 15);
}
