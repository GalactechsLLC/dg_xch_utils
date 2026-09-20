use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_farmer::farmer::PathInfo;
use dg_xch_farmer::farmer::config::{Config, FarmingInfo};
use std::collections::HashMap;

#[test]
fn same_named_plots_on_different_disks_have_separate_identifiers() {
    let first = PathInfo::new("disk-one/same.plot".into());
    let second = PathInfo::new("disk-two/same.plot".into());
    assert_ne!(first, second);
    let plots = HashMap::from([(first.clone(), 1), (second.clone(), 2)]);
    assert_eq!(plots.get(first.identifier()), Some(&1));
    assert_eq!(plots.get(second.identifier()), Some(&2));
    assert_eq!(plots.get("unknown"), None);
}

#[test]
fn network_configuration_never_falls_back_to_mainnet() {
    let mut config: Config<()> = Config {
        selected_network: "typo".into(),
        ..Default::default()
    };
    assert!(config.constants().is_err());
    assert!(!config.is_ready());
    config.selected_network = "dgx".into();
    config.chain_definition = Some(ChainDefinition::default());
    let constants = config.constants().unwrap();
    assert_eq!(constants.rewards.genesis_farmer, 0);
    assert_eq!(constants.rewards.genesis_pool, 0);
    assert_eq!(
        config.network_id().unwrap(),
        config
            .chain_definition
            .as_ref()
            .unwrap()
            .handshake_network_id()
            .unwrap()
    );
}

#[test]
fn invalid_keys_are_rejected_and_debug_is_redacted() {
    let config: Config<()> = Config {
        farmer_info: vec![FarmingInfo::default()],
        ..Default::default()
    };
    assert!(config.validate_keys().is_err());
    assert!(!format!("{:?}", config.farmer_info[0]).contains("farmer_secret_key"));
}

#[test]
fn legacy_configuration_is_saved_privately() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("farmer.yaml");
    let config: Config<()> = Config::default();
    config.save_as_yaml(&path).unwrap();
    assert_eq!(Config::<()>::try_from(path.as_path()).unwrap(), config);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}
