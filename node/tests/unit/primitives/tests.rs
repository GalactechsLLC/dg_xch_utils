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
        falcon_signatures: Vec::new(),
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

#[test]
fn common_authorization_seam_accepts_bls_and_rejects_wrong_witness_modes() {
    let native = NativePrimitives;
    let mut infinity = [0u8; 96];
    infinity[0] = 0xc0;
    let infinity = Bytes96::from(infinity);
    native
        .verify_block_authorization(
            &empty_conditions(),
            &AuthorizationWitness::BlsAggregate(&infinity),
            &MAINNET,
        )
        .unwrap();
    for witness in [
        AuthorizationWitness::FalconDirect(&[]),
        AuthorizationWitness::FalconLabrador(&[]),
        AuthorizationWitness::BlsAggregate(&Bytes96::default()),
    ] {
        assert_eq!(
            native.verify_block_authorization(&empty_conditions(), &witness, &MAINNET),
            Err(ChiaError::BadAggregateSignature),
        );
    }
}
