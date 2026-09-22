use dg_xch_core::consensus::chain_definition::ChainDefinition;
use dg_xch_farmer::farmer::PathInfo;
use dg_xch_farmer::farmer::config::{Config, FarmingInfo, Pos2Backend, Pos2HarvesterConfig};
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
fn named_dgx_and_default_chia_resolve_without_network_fallback() {
    let mut config: Config<()> = Config::default();
    assert_eq!(config.network_id().unwrap(), "mainnet");
    config.selected_network = "dgx".into();
    assert!(config.network_id().unwrap().starts_with("dgx-"));
    assert_eq!(config.constants().unwrap().hard_fork2_height, 0);
    config.selected_network = "dgx-typo".into();
    assert!(config.constants().is_err());
}

#[test]
fn pos2_resource_and_backend_configuration_fails_closed() {
    let mut config = Pos2HarvesterConfig::default();
    assert!(config.validate().is_ok());
    config.backend = Pos2Backend::Cuda;
    assert!(config.validate().is_err());
    config.cuda_helper = Some(std::env::temp_dir().join("dg_xch_pos2_cuda"));
    assert!(config.validate().is_ok());
    config.memory_mib = u64::MAX;
    assert!(config.validate().is_err());
    config.memory_mib = 1024;
    config.parallelism = 0;
    assert!(config.validate().is_err());
    config.parallelism = 1;
    config.deadline_ms = 0;
    assert!(config.validate().is_err());
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
