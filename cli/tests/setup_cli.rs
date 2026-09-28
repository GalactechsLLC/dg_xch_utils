use std::process::{Command, Output};

fn invoke(root: &std::path::Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dgx"))
        .arg("--config-dir")
        .arg(root)
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn initialization_is_persistent_repeatable_and_non_destructive() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    let data = directory.path().join("data");
    let plots = directory.path().join("plots");
    let output = invoke(
        &config,
        &[
            "init",
            "--non-interactive",
            "--data-dir",
            data.to_str().unwrap(),
            "--plots-dir",
            plots.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let profile = dg_xch_servers::app_config::AppConfig::load(&config).unwrap();
    assert_eq!(profile.data_dir, data);
    assert_eq!(profile.plot_directories, vec![plots]);
    let key_path = config.join("ssl/ca/private_ca.key");
    let key = std::fs::read(&key_path).unwrap();
    let desktop = std::fs::read(config.join("desktop.json")).unwrap();
    let settings: serde_json::Value = serde_json::from_slice(&desktop).unwrap();
    assert_eq!(settings["node_port"], 8444);
    assert_eq!(settings["network"], "mainnet");
    assert_eq!(
        settings["genesis_header_hash"],
        "d780d22c7a87c9e01d98b49a0910f6701c3b95015741316b3fda042e5d7b81d2"
    );
    assert!(
        invoke(&config, &["init", "--non-interactive"])
            .status
            .success()
    );
    assert_eq!(std::fs::read(key_path).unwrap(), key);
    assert_eq!(std::fs::read(config.join("desktop.json")).unwrap(), desktop);
    assert!(
        !invoke(
            &config,
            &[
                "init",
                "--non-interactive",
                "--data-dir",
                directory.path().to_str().unwrap()
            ]
        )
        .status
        .success()
    );
}

#[test]
fn services_require_initialization_but_help_does_not() {
    let directory = tempfile::tempdir().unwrap();
    assert!(invoke(directory.path(), &["--help"]).status.success());
    for service in [
        "gui",
        "farmer",
        "plotter",
        "timelord",
        "introducer",
        "simulator",
    ] {
        let output = invoke(directory.path(), &[service]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("run dgx init first"));
    }
    #[cfg(feature = "full-node")]
    {
        let output = invoke(directory.path(), &["full-node"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("run dgx init first"));
        assert!(
            invoke(directory.path(), &["full-node", "--print-chain-info"])
                .status
                .success()
        );
    }
}

#[test]
fn non_terminal_setup_requires_explicit_automation_flag() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(directory.path(), &["init"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--non-interactive"));
    assert!(!directory.path().join("dgx.json").exists());
}

#[test]
fn application_services_run_without_companion_binaries() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory
        .path()
        .join(format!("dgx{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_dgx"), &executable).unwrap();
    let profile = dg_xch_servers::app_config::AppConfig {
        version: 1,
        data_dir: directory.path().join("data"),
        plot_directories: vec![directory.path().join("plots")],
    };
    profile.save_new(directory.path()).unwrap();
    let mut services = vec!["farmer", "plotter", "introducer"];
    if cfg!(feature = "timelord") {
        services.push("timelord");
    }
    for service in services {
        let output = Command::new(&executable)
            .arg("--config-dir")
            .arg(directory.path())
            .args([service, "--", "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{service}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
    }
}

#[test]
fn launcher_preserves_service_flags() {
    use clap::Parser;
    use dg_xch_cli_lib::cli::{Cli, RootCommands};
    let parsed = Cli::try_parse_from([
        "dgx",
        "plotter",
        "create",
        "--k",
        "28",
        "--output",
        "a plot.plot",
    ])
    .unwrap();
    let RootCommands::Plotter(arguments) = parsed.action else {
        panic!("wrong command")
    };
    assert_eq!(
        arguments.arguments,
        ["create", "--k", "28", "--output", "a plot.plot"]
    );
}
