use dg_xch_core::protocols::pool::{
    get_current_authentication_token, validate_authentication_token,
};

#[test]
fn zero_pool_timeout_is_rejected_without_panicking() {
    assert!(get_current_authentication_token(0).is_err());
    assert!(!validate_authentication_token(0, 0));
    let token = get_current_authentication_token(5).unwrap();
    assert!(validate_authentication_token(token, 5));
}
#[cfg(feature = "falcon")]
#[test]
fn falcon_pool_authentication_binds_chain_pool_operation_owner_and_payload() {
    use dg_xch_core::{
        protocols::pool_falcon as wire,
        signatures::{SignatureSigner, falcon::FalconSigner},
    };
    let key = FalconSigner::generate().unwrap();
    let other = FalconSigner::generate().unwrap();
    let genesis = [1; 32].into();
    let target = [2; 32].into();
    let payload = wire::Registration {
        launcher_id: [3; 32].into(),
        payout_puzzle_hash: [4; 32].into(),
        timestamp: 1000,
    };
    let request = wire::sign(&key, genesis, target, "register", payload.clone()).unwrap();
    let public = wire::PublicKey(key.public_key());
    wire::verify(
        &request.authorization,
        &public,
        genesis,
        target,
        "register",
        &payload,
    )
    .unwrap();
    for (chain, pool, operation) in [
        ([9; 32].into(), target, "register"),
        (genesis, [9; 32].into(), "register"),
        (genesis, target, "get_farmer"),
    ] {
        assert!(
            wire::verify(
                &request.authorization,
                &public,
                chain,
                pool,
                operation,
                &payload
            )
            .is_err()
        );
    }
    assert!(
        wire::verify(
            &request.authorization,
            &wire::PublicKey(other.public_key()),
            genesis,
            target,
            "register",
            &payload
        )
        .is_err()
    );
    let mut changed = payload.clone();
    changed.payout_puzzle_hash = [9; 32].into();
    assert!(
        wire::verify(
            &request.authorization,
            &public,
            genesis,
            target,
            "register",
            &changed
        )
        .is_err()
    );
    let mut malformed = request.authorization.clone();
    malformed.signature[100] ^= 1;
    assert!(wire::verify(&malformed, &public, genesis, target, "register", &payload).is_err());
    assert!(wire::timestamp(1000, 1060).is_ok());
    assert!(wire::timestamp(1000, 1061).is_err());
    assert!(wire::timestamp(u64::MAX, 1000).is_err());
}
