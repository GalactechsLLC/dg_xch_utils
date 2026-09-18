use super::*;

#[test]
fn config_error_names_the_field_and_the_tier() {
    let e = ConfigError::range("consensus.num_sps_sub_slot", "must be in 1..=128, got 0");
    let msg = e.to_string();
    assert!(msg.contains("range"), "{msg}");
    assert!(msg.contains("consensus.num_sps_sub_slot"), "{msg}");
    assert!(msg.contains("got 0"), "{msg}");

    let x = ConfigError::cross_field(
        "consensus.epoch_blocks/consensus.sub_epoch_blocks",
        "epoch_blocks must be a whole multiple of sub_epoch_blocks",
    );
    assert_eq!(x.tier, ValidationTier::CrossField);
    assert!(x.to_string().contains("cross-field"), "{x}");
}

#[test]
fn tier_strings_are_stable() {
    assert_eq!(ValidationTier::Type.as_str(), "type");
    assert_eq!(ValidationTier::Range.as_str(), "range");
    assert_eq!(ValidationTier::CrossField.as_str(), "cross-field");
}

#[test]
fn sim_error_keeps_the_config_error_as_its_source() {
    let e: SimError = ConfigError::typed("harness.n_runs", "expected a positive integer").into();
    assert!(e.to_string().starts_with("invalid config:"), "{e}");
    let src = e.source().expect("config error is the source");
    assert!(src.to_string().contains("harness.n_runs"), "{src}");
}

#[test]
fn invariant_violations_have_no_source() {
    let e = SimError::Invariant("weight trajectory diverged at height 41".to_string());
    assert!(e.to_string().contains("shared invariant violated"), "{e}");
    assert!(e.source().is_none());
}
