pub mod wallet_commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(short, long, value_name = "Path to the chia ssl folder")]
    ssl_path: Option<PathBuf>,

    #[arg(long, value_name = "Fullnode Hostname")]
    fullnode_host: Option<String>,
    #[arg(long, value_name = "Fullnode Port")]
    fullnode_port: Option<u16>,

    #[arg(long, value_name = "Wallet Hostname")]
    wallet_host: Option<String>,
    #[arg(long, value_name = "Wallet Port")]
    wallet_port: Option<u16>,

    #[command(subcommand)]
    pub action: RootCommands,
}

#[derive(Debug, Subcommand)]
pub enum RootCommands {
    #[command(about = "Gets coin records for a given address or puzzlehash", long_about = None)]
    GetCoinRecord {
        #[arg(short, long)]
        puzzlehash: Option<String>,
        address: Option<String>,
        include_spent_coins: bool,
    },
    #[command(about = "Create a cold wallet or a PlotNFT wallet", long_about = None)]
    CreateWallet {
        #[command(subcommand)]
        action: WalletAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum WalletAction {
    #[command(about = "Creates a wallet with a plotnft", long_about = None)]
    WithNFT {
        #[arg(short, long)]
        pool_url: Option<String>,
        faucet_request_url: Option<String>,
        faucet_request_payload: Option<String>,
    },
    #[command(about = "Creates a Cold wallet", long_about = None)]
    Cold,
}
