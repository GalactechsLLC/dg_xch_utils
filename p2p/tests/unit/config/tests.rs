use super::*;

#[test]
fn defaults_are_valid() {
    assert!(P2pSettings::default().validate().is_ok());
}

#[test]
fn invalid_limits_are_rejected() {
    let mut settings = P2pSettings::default();
    settings.target_outbound = settings.target_peer_count + 1;
    assert!(settings.validate().is_err());

    settings = P2pSettings::default();
    settings.address_lower = settings.address_upper + 1;
    assert!(settings.validate().is_err());

    settings = P2pSettings::default();
    settings.jitter_floor = f64::NAN;
    assert!(settings.validate().is_err());
}
