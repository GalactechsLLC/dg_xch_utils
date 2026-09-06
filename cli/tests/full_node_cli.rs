use clap::Parser;
use dg_xch_cli_lib::cli::{Cli, RootCommands};

#[test]
fn confirmation_performance_controls_parse() {
    let cli = Cli::try_parse_from([
        "dg",
        "full-node",
        "--compute-workers=32",
        "--validation-window-blocks=256",
        "--validation-window-mb=64",
        "--confirm-transaction-blocks=64",
        "--confirm-transaction-coin-changes=50000",
        "--confirm-transaction-coin-mb=32",
        "--sqlite-writer-cache-mb=1024",
        "--coalesce-coin-writes",
        "--prefetch-memory-mb=1024",
        "--prefetch-max-inflight=64",
    ])
    .unwrap();
    assert!(matches!(cli.action, RootCommands::FullNode(_)));
}

#[test]
fn full_node_is_a_dg_subcommand() {
    let cli = Cli::try_parse_from([
        "dg",
        "full-node",
        "--listen",
        "127.0.0.1:8444",
        "--rpc",
        "127.0.0.1:8444",
        "--db",
        "sqlite:///tmp/chain.db",
        "--peer",
        "node.example:8444",
    ])
    .expect("parse full-node command");

    assert!(matches!(cli.action, RootCommands::FullNode(_)));
}
