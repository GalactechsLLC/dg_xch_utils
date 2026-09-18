use super::*;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::traits::SizedBytes;

fn empty_conditions() -> SpendBundleConditions {
    SpendBundleConditions {
        spends: Vec::new(),
        reserve_fee: 0,
        height_absolute: 0,
        seconds_absolute: 0,
        before_height_absolute: None,
        before_seconds_absolute: None,
        agg_sig_unsafe: Vec::new(),
        cost: 0,
        removal_amount: 0,
        addition_amount: 0,
    }
}

fn verify_sig<P: ConsensusPrimitives>(
    primitives: &P,
    conditions: &SpendBundleConditions,
    aggregate: &Bytes96,
    constants: &ConsensusConstants,
) -> Result<(), ChiaError> {
    primitives.verify_block_aggregate_signature(conditions, aggregate, constants)
}

#[test]
fn engine_binds_to_seam_not_concrete() {
    let native = NativePrimitives;
    // An empty spend set with a non-infinity aggregate must be rejected.
    let non_infinity = Bytes96::parse(&[0xab_u8; 96]).unwrap();
    let result = verify_sig(&native, &empty_conditions(), &non_infinity, &MAINNET);
    assert!(matches!(result, Err(ChiaError::BadAggregateSignature)));
}
