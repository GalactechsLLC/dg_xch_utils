use clap::Parser;
use dg_xch_cli_lib::cli::Cli;
#[cfg(feature = "full-node")]
use dg_xch_cli_lib::cli::RootCommands;

#[test]
#[cfg(feature = "full-node")]
fn confirmation_performance_controls_parse() {
    let cli = Cli::try_parse_from([
        "dgx",
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
#[cfg(feature = "full-node")]
fn full_node_is_a_dgx_subcommand() {
    let cli = Cli::try_parse_from([
        "dgx",
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

#[test]
#[cfg(not(feature = "full-node"))]
fn wallet_only_cli_omits_full_node_but_keeps_rpc_commands() {
    assert!(Cli::try_parse_from(["dgx", "full-node"]).is_err());
    assert!(Cli::try_parse_from(["dgx", "get-blockchain-state"]).is_ok());
}
