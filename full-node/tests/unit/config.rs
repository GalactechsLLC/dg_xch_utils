use super::*;

#[test]
fn performance_bounds_reject_zero_and_unbounded_work() {
    assert!(PerformanceConfig::default().validate().is_ok());
    for invalid in [
        PerformanceConfig {
            compute_workers: Some(0),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            compute_workers: Some(1025),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            validation_window_blocks: Some(0),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            validation_window_blocks: Some(4097),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            validation_window_mb: 0,
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            confirm_transaction_blocks: Some(0),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            confirm_transaction_coin_changes: Some(0),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            confirm_transaction_coin_changes: Some(100_000_001),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            confirm_transaction_coin_mb: Some(0),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            confirm_transaction_coin_mb: Some(65_537),
            ..PerformanceConfig::default()
        },
        PerformanceConfig {
            sqlite_writer_cache_mb: Some(0),
            ..PerformanceConfig::default()
        },
    ] {
        assert!(invalid.validate().is_err());
    }
}

fn cfg(peers: &[&str]) -> Result<Config, String> {
    let owned: Vec<String> = peers.iter().map(|s| (*s).to_string()).collect();
    Config::build(
        "0.0.0.0:8444",
        "0.0.0.0:8555",
        None,
        &owned,
        None,
        "sqlite:///data/chain.db",
        "mainnet",
        None,
        false,
        0,
        false,
        None,
        None,
        P2pSettings::default(),
        &[],
        &[],
    )
}

#[test]
fn trusted_peers_default_empty_and_pass_through() {
    assert!(cfg(&[]).unwrap().trusted_peers.is_empty());
    assert!(cfg(&[]).unwrap().trusted_cidrs.is_empty());
    let c = Config::build(
        "0.0.0.0:8444",
        "0.0.0.0:8555",
        None,
        &[],
        None,
        "sqlite:///data/chain.db",
        "mainnet",
        None,
        false,
        0,
        false,
        None,
        None,
        P2pSettings::default(),
        &["aa".repeat(32)],
        &["10.0.0.0/8".to_string()],
    )
    .unwrap();
    assert_eq!(c.trusted_peers, vec!["aa".repeat(32)]);
    assert_eq!(c.trusted_cidrs, vec!["10.0.0.0/8".to_string()]);
}

#[test]
fn no_peer_flags_yield_empty_manual_peers() {
    assert!(cfg(&[]).unwrap().manual_peers.is_empty());
}

#[test]
fn repeated_peer_flags_all_parse_in_order() {
    // A DNS service name and a bare IP must both parse; order is preserved.
    let c = cfg(&["chia-node-0.peers.example:8444", "10.101.159.8:8444"]).unwrap();
    assert_eq!(
        c.manual_peers,
        vec![
            ("chia-node-0.peers.example".to_string(), 8444),
            ("10.101.159.8".to_string(), 8444),
        ]
    );
}

#[test]
fn a_peer_without_a_port_is_rejected() {
    assert!(cfg(&["chia-node-0.peers"]).is_err());
}

#[test]
fn p2p_settings_are_validated_and_preserved() {
    let mut p2p = P2pSettings {
        host_pool_capacity: 2_000,
        heartbeat: std::time::Duration::from_secs(30),
        ..P2pSettings::default()
    };
    let c = Config::build(
        "0.0.0.0:8444",
        "127.0.0.1:8555",
        None,
        &[],
        None,
        "sqlite:///data/chain.db",
        "mainnet",
        None,
        false,
        0,
        false,
        None,
        None,
        p2p,
        &[],
        &[],
    )
    .unwrap();
    assert_eq!(c.p2p, p2p);

    p2p.address_lower = p2p.address_upper + 1;
    assert!(
        Config::build(
            "0.0.0.0:8444",
            "127.0.0.1:8555",
            None,
            &[],
            None,
            "sqlite:///data/chain.db",
            "mainnet",
            None,
            false,
            0,
            false,
            None,
            None,
            p2p,
            &[],
            &[],
        )
        .is_err()
    );
}
