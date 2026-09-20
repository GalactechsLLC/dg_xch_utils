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
