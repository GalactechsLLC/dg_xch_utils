use clap::Parser;
use dg_xch_cli_lib::cli::{Cli, RootCommands};

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
