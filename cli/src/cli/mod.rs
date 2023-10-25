use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(short, long, value_name = "Path to the chia ssl folder")]
    pub ssl_path: Option<String>,

    #[arg(long, value_name = "Fullnode Hostname")]
    pub fullnode_host: Option<String>,
    #[arg(long, value_name = "Fullnode Port")]
    pub fullnode_port: Option<u16>,

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
        #[arg(short, long)]
        address: Option<String>,
        #[arg(short, long)]
        include_spent_coins: bool,
    },
    #[command(about = "Migrates a PlotNFT using a mnemonic", long_about = None)]
    MovePlotNFT {
        #[arg(short, long)]
        target_pool: String,
        #[arg(short, long)]
        launcher_id: String,
        #[arg(short, long)]
        mnemonic: String,
        #[arg(short, long)]
        fee: Option<u64>,
    },
    #[command(about = "Migrates a PlotNFT using an owner_secrey_key", long_about = None)]
    MovePlotNFTWithOwnerKey {
        #[arg(short, long)]
        target_pool: String,
        #[arg(short, long)]
        launcher_id: String,
        #[arg(short, long)]
        owner_key: String,
    },
    #[command(about = "Gets plotnft state for launcher_id", long_about = None)]
    GetPlotnftState {
        #[arg(short, long)]
        launcher_id: String,
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
