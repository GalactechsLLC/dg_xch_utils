use super::*;
use crate::consensus::constants::MAINNET;

#[test]
fn genesis_is_min_minus_one() {
    assert_eq!(calculate_deficit(&MAINNET, 0, None, false, 0), 15);
}
