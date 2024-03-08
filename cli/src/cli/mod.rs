use bip39::Mnemonic;
use clap::{Parser, Subcommand};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dialoguer::theme::ColorfulTheme;
use dialoguer::Input;
use std::io::{Error, ErrorKind};
use std::str::FromStr;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[arg(short, long, value_name = "Path to the chia ssl folder")]
    pub ssl_path: Option<String>,
    #[arg(short, long, value_name = "Timeout When Connecting to Fullnode")]
    pub timeout: Option<u64>,

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
    //START OF FULLNODE API
    #[command(about = "Get the current BlockchainState", long_about = None)]
    PrintPlottingInfo {
        #[arg(long)]
        launcher_id: Option<Bytes32>,
    },
    //START OF FULLNODE API
    #[command(about = "Get the current BlockchainState", long_about = None)]
    GetBlockchainState,
    #[command(about = "Loads a FullBlock by header_hash", long_about = None)]
    GetBlock {
        #[arg(long)]
        header_hash: Bytes32,
    },
    #[command(about = "Loads current BlockStateMetrics", long_about = None)]
    GetBlockCountMetrics,
    #[command(about = "Loads FullBlocks between start and end", long_about = None)]
    GetBlocks {
        #[arg(long)]
        start: u32,
        #[arg(long)]
        end: u32,
        #[arg(long)]
        exclude_header_hash: bool,
        #[arg(long)]
        exclude_reorged: bool,
    },
    #[command(about = "Loads all FullBlocks between start and end", long_about = None)]
    GetAllBlocks {
        #[arg(long)]
        start: u32,
        #[arg(long)]
        end: u32,
    },
    #[command(about = "Loads a BlockRecord by header_hash", long_about = None)]
    GetBlockRecord {
        #[arg(long)]
        header_hash: Bytes32,
    },
    #[command(about = "Loads a BlockRecord by height", long_about = None)]
    GetBlockRecordByHeight {
        #[arg(long)]
        height: u32,
    },
    #[command(about = "Loads all BlockRecords between start and end", long_about = None)]
    GetBlockRecords {
        #[arg(long)]
        start: u32,
        #[arg(long)]
        end: u32,
    },
    #[command(about = "Loads UnfinishedBlocks", long_about = None)]
    GetUnfinishedBlocks,
    #[command(about = "Get Est network Space between two header_hashes", long_about = None)]
    GetNetworkSpace {
        #[arg(long)]
        older_block_header_hash: Bytes32,
        #[arg(long)]
        newer_block_header_hash: Bytes32,
    },
    #[command(about = "Get Est network Space between two heights", long_about = None)]
    GetNetworkSpaceaByHeight {
        #[arg(long)]
        start: u32,
        #[arg(long)]
        end: u32,
    },
    #[command(about = "Get additions and removals by header_hash", long_about = None)]
    GetAdditionsAndRemovals {
        #[arg(long)]
        header_hash: Bytes32,
    },
    #[command(about = "Loads InitialFreezePeriod", long_about = None)]
    GetInitialFreezePeriod,
    #[command(about = "Loads InitialFreezePeriod", long_about = None)]
    GetNetworkInfo,
    #[command(about = "Get SignagePoint or End Of Subslot, Only Provide one of sp_hash or challenge_hash", long_about = None)]
    GetSignagePointOrEOS {
        #[arg(long)]
        sp_hash: Option<Bytes32>,
        #[arg(long)]
        challenge_hash: Option<Bytes32>,
    },
    #[command(about = "Get CoinRecords by puzzle_hashs", long_about = None)]
    GetCoinRecords {
        #[arg(long)]
        puzzle_hashes: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: Option<bool>,
        #[arg(long)]
        start_height: Option<u32>,
        #[arg(long)]
        end_height: Option<u32>,
    },
    #[command(about = "Get CoinRecord by name", long_about = None)]
    GetCoinRecordByName {
        #[arg(long)]
        name: Bytes32,
    },
    #[command(about = "Get CoinRecords by names", long_about = None)]
    GetCoinRecordsByNames {
        #[arg(long)]
        names: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: bool,
        #[arg(long)]
        start_height: u32,
        #[arg(long)]
        end_height: u32,
    },
    #[command(about = "Get CoinRecords by parent ids", long_about = None)]
    GetCoinRecordsByParentIds {
        #[arg(long)]
        parent_ids: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: bool,
        #[arg(long)]
        start_height: u32,
        #[arg(long)]
        end_height: u32,
    },
    #[command(about = "Get CoinRecords by hint", long_about = None)]
    GetCoinRecordsByhint {
        #[arg(long)]
        hint: Bytes32,
        #[arg(long)]
        include_spent_coins: bool,
        #[arg(long)]
        start_height: u32,
        #[arg(long)]
        end_height: u32,
    },
    #[command(about = "Get CoinSpend for coin_id at height", long_about = None)]
    GetPuzzleAndSolution {
        #[arg(long)]
        coin_id: Bytes32,
        #[arg(long)]
        height: u32,
    },
    #[command(about = "Get CoinSpend for coin_id at height", long_about = None)]
    GetCoinSpend {
        #[arg(long)]
        coin_id: Bytes32,
        #[arg(long)]
        height: u32,
    },
    #[command(about = "Loads All Mempool Transaction Ids", long_about = None)]
    GetAllMempoolTxIds,
    #[command(about = "Loads All Mempool Items", long_about = None)]
    GetAllMempoolItems,
    #[command(about = "Get MempoolItem with TxID", long_about = None)]
    GetMempoolItemByTxID {
        #[arg(long)]
        tx_id: String,
    },
    #[command(about = "Get MempoolItem by name", long_about = None)]
    GetMempoolItemByName {
        #[arg(long)]
        coin_name: Bytes32,
    },
    #[command(about = "Get MempoolItem by name", long_about = None)]
    GetFeeEstimate {
        #[arg(long)]
        cost: Option<u64>,
        #[arg(long)]
        spend_bundle: Option<String>,
        #[arg(long)]
        spend_type: Option<String>,
        #[arg(long)]
        target_times: Vec<u64>,
    },
    //END FULLNODE API
    //START EXTENDED FULLNODEAPI
    #[command(about = "Get Singleton by LauncherID", long_about = None)]
    GetSingletonByLauncherId {
        #[arg(long)]
        launcher_id: Bytes32,
    },
    #[command(about = "Get additions and removals with hints by header_hash", long_about = None)]
    GetAdditionsAndRemovalsWithHints {
        #[arg(long)]
        header_hash: Bytes32,
    },
    #[command(about = "Get CoinRecords by hint", long_about = None)]
    GetCoinRecordsByHints {
        #[arg(long)]
        hints: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: bool,
        #[arg(long)]
        start_height: u32,
        #[arg(long)]
        end_height: u32,
    },
    #[command(about = "Get CoinRecords by hint", long_about = None)]
    GetCoinRecordsByHintsPaginated {
        #[arg(long)]
        hints: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: Option<bool>,
        #[arg(long)]
        start_height: Option<u32>,
        #[arg(long)]
        end_height: Option<u32>,
        #[arg(long)]
        page_size: u32,
        #[arg(long)]
        last_id: Option<Bytes32>,
    },
    #[command(about = "Get CoinRecords by hint", long_about = None)]
    GetCoinRecordsByPuzzleHashesPaginated {
        #[arg(long)]
        puzzle_hashes: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: Option<bool>,
        #[arg(long)]
        start_height: Option<u32>,
        #[arg(long)]
        end_height: Option<u32>,
        #[arg(long)]
        page_size: u32,
        #[arg(long)]
        last_id: Option<Bytes32>,
    },
    #[command(about = "Get Hints by CoinIds", long_about = None)]
    GetHintsByCoinIds {
        #[arg(long)]
        coin_ids: Vec<Bytes32>,
    },
    #[command(about = "Get Hints by CoinIds", long_about = None)]
    GetPuzzleAndSoultionsByNames {
        #[arg(long)]
        names: Vec<Bytes32>,
        #[arg(long)]
        include_spent_coins: Option<bool>,
        #[arg(long)]
        start_height: Option<u32>,
        #[arg(long)]
        end_height: Option<u32>,
    },
    //END EXTENDED FULLNODE API
    #[command(about = "Migrates a PlotNFT using a mnemonic", long_about = None)]
    MovePlotNFT {
        #[arg(long)]
        target_pool: String,
        #[arg(long)]
        launcher_id: Bytes32,
        #[arg(long)]
        mnemonic: String,
        #[arg(long)]
        fee: Option<u64>,
    },
    #[command(about = "Migrates a PlotNFT using an owner_secrey_key", long_about = None)]
    MovePlotNFTWithOwnerKey {
        #[arg(long)]
        target_pool: String,
        #[arg(long)]
        launcher_id: Bytes32,
        #[arg(long)]
        owner_key: String,
    },
    #[command(about = "Gets plotnft state for launcher_id", long_about = None)]
    GetPlotnftState {
        #[arg(long)]
        launcher_id: Bytes32,
    },
    #[command(about = "Create Login link for Pool", long_about = None)]
    CreatePoolLoginLink {
        #[arg(short, long)]
        target_pool: String,
        #[arg(short, long)]
        launcher_id: Bytes32,
        #[arg(short, long)]
        auth_key: Bytes32,
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
        #[arg(long)]
        pool_url: Option<String>,
        faucet_request_url: Option<String>,
        faucet_request_payload: Option<String>,
    },
    #[command(about = "Creates a Cold wallet", long_about = None)]
    Cold,
}

pub fn prompt_for_mnemonic() -> Result<Mnemonic, Error> {
    Mnemonic::from_str(
        &Input::<String>::with_theme(&ColorfulTheme::default())
            .with_prompt("Please Input Your Mnemonic: ")
            .validate_with(|input: &String| -> Result<(), &str> {
                if Mnemonic::from_str(input).is_ok() {
                    Ok(())
                } else {
                    Err("You did not input a valid Mnemonic, Please try again.")
                }
            })
            .interact_text()
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to read user Input for Mnemonic: {e:?}"),
                )
            })?,
    )
    .map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Failed to parse Mnemonic: {e:?}"),
        )
    })
}
