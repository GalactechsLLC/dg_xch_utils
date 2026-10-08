use std::process::Command;

fn dgx() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dgx"));
    // Each subprocess has its own environment; tests never mutate the process environment.
    command.env_clear();
    command
}

#[cfg(feature = "full-node")]
#[test]
fn full_node_environment_selects_network_and_cli_takes_precedence() {
    for (args, expected) in [
        (vec!["full-node", "--print-chain-info"], "testnet11"),
        (
            vec!["full-node", "--print-chain-info", "--network", "mainnet"],
            "mainnet",
        ),
    ] {
        let output = dgx()
            .env("DGX_FULL_NODE_NETWORK", "testnet11")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(info["network_id"], expected);
    }
}

#[test]
fn farmer_reports_service_configuration_without_requiring_dgx_json() {
    let directory = tempfile::tempdir().unwrap();
    let output = dgx()
        .env("DGX_CONFIG_DIR", directory.path())
        .args(["farmer"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("complete configuration in DGX_FARMER_CONFIG"),
        "{error}"
    );
    assert!(!error.contains("run dgx init"), "{error}");
}

#[test]
fn environment_document_loading_and_explicit_file_override_work_for_introducer() {
    let contents = serde_json::json!({
        "listen": "0.0.0.0:8445", "chain": "mainnet",
        "tls": {"certificate": "/unused/cert", "private_key": "/unused/key", "ca_certificate": "/unused/ca"},
        "peer_server_name": "localhost", "max_connections": 0,
        "max_peers": 4096, "peer_ttl_seconds": 3600
    }).to_string();
    // Validation follows deserialization, proving the complete document was loaded.
    let output = dgx()
        .env("DGX_INTRODUCER_CONFIG", &contents)
        .args(["introducer"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("invalid introducer resource limits"),
        "{error}"
    );
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("config.json");
    std::fs::write(&file, contents).unwrap();
    let output = dgx()
        .env("DGX_INTRODUCER_CONFIG", "malformed env")
        .arg("introducer")
        .arg("--config")
        .arg(file)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid introducer resource limits"));
}

#[test]
fn multiline_farmer_config_is_parsed_from_environment_without_a_file() {
    let config = dg_xch_farmer::farmer::config::Config::<()>::default();
    let contents = serde_yaml::to_string(&config).unwrap();
    assert!(contents.contains('\n'));
    let output = dgx()
        .env("DGX_FARMER_CONFIG", contents)
        .args(["farmer"])
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("farmer configuration is incomplete or has an unknown network"),
        "{error}"
    );
    assert!(!error.contains("run dgx init"), "{error}");
}

#[test]
fn plotting_keys_can_be_supplied_through_environment() {
    let output = dgx()
        .env("DGX_PLOTTER_OUTPUT", "/unused/test.plot")
        .env("DGX_PLOTTER_FARMER_KEY", "invalid-key")
        .env("DGX_PLOTTER_POOL_KEY", "invalid-key")
        .args(["plotter", "create"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.contains("required arguments"), "{error}");
    assert!(!error.contains("run dgx init"), "{error}");
}

#[test]
fn service_help_explains_environment_configuration_without_extra_separators() {
    let output = dgx().args(["farmer", "--help"]).output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("DGX_FARMER_CONFIG"), "{help}");
    assert!(help.contains("contains the document itself"), "{help}");
}

#[cfg(feature = "full-node")]
#[test]
fn full_node_loads_yaml_documents_and_explicit_cli_flags_win() {
    let document = "network: testnet11\nlisten: 0.0.0.0:8444\ndb: sqlite:///data/chain.db\nssl_dir: /data/ssl\ncompute_workers: 1\n";
    for (args, network) in [
        (vec!["full-node", "--print-chain-info"], "testnet11"),
        (
            vec!["full-node", "--print-chain-info", "--network", "mainnet"],
            "mainnet",
        ),
    ] {
        let output = dgx()
            .env("DGX_FULL_NODE_CONFIG", document)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(info["network_id"], network);
    }
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("node.yaml");
    std::fs::write(&file, document).unwrap();
    let output = dgx()
        .env("DGX_FULL_NODE_CONFIG", "malformed env")
        .args(["full-node", "--print-chain-info", "--config"])
        .arg(file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["network_id"],
        "testnet11"
    );
}

#[cfg(feature = "full-node")]
#[test]
fn full_node_rejects_unknown_fields_types_and_conflicting_networks() {
    for document in [
        "network: not-a-network",
        "unknown_setting: 1",
        "compute_workers: invalid",
        "[]",
    ] {
        let output = dgx()
            .env("DGX_FULL_NODE_CONFIG", document)
            .args(["full-node", "--print-chain-info"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {document}");
    }
    let output = dgx()
        .env("DGX_FULL_NODE_CONFIG", "network: testnet11")
        .args(["--network", "mainnet", "full-node", "--print-chain-info"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
