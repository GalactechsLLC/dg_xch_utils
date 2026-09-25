use crate::cli::ProgramOutput;
use crate::wallet_commands::{
    create_cold_wallet, get_plotnft_ready_state, migrate_plot_nft, migrate_plot_nft_with_owner_key,
};
use crate::wallets::plotnft_utils::{get_plotnft_by_launcher_id, scrounge_for_plotnfts};
use blst::min_pk::SecretKey;
use clap::Parser;
use cli::{Cli, RootCommands, WalletAction, prompt_for_mnemonic};
use dg_logger::DruidGardenLogger;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::api::pool::create_pool_login_url;
use dg_xch_clients::rpc::full_node::{
    FullnodeAPI, FullnodeClient, FullnodeExtAPI, FullnodeHelpers,
};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::assemble::{assemble_text, is_hex};
use dg_xch_core::clvm::parser::sexp_to_bytes;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::utils::INFINITE_COST;
use dg_xch_core::consensus::chain_definition::ChainSelection;
use dg_xch_keys::{
    encode_puzzle_hash, key_from_mnemonic, master_sk_to_farmer_sk, master_sk_to_pool_sk,
    master_sk_to_wallet_sk, master_sk_to_wallet_sk_unhardened,
};
use dg_xch_puzzles::clvm_puzzles::launcher_id_to_p2_puzzle_hash;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::{decode, encode};
use log::{Level, error, info};
use std::env;
use std::io::{Cursor, Error, ErrorKind};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

pub mod chain;
pub mod cli;
pub mod commands;
#[cfg(feature = "full-node")]
pub mod full_node;
pub mod services;
pub mod setup;
pub mod simulator;
pub mod wallet_commands;
pub mod wallets;

fn inherit_network(network: &mut Option<String>, root_network: Option<&str>) -> Result<(), Error> {
    if let Some(root_network) = root_network {
        if network
            .as_deref()
            .is_some_and(|network| network != root_network)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "root --network conflicts with subcommand --network",
            ));
        }
        *network = Some(root_network.to_owned());
    }
    Ok(())
}

fn apply_network(cli: &mut Cli) -> Result<(), Error> {
    match &mut cli.action {
        RootCommands::Chain(args) => args.inherit_network(cli.network.as_deref()),
        #[cfg(feature = "full-node")]
        RootCommands::FullNode(args) => args.inherit_network(cli.network.as_deref()),
        _ => {
            if let Some(network) = cli.network.as_deref() {
                ChainSelection::from_config(network, None)
                    .map_err(|error| Error::new(ErrorKind::InvalidInput, error))?;
            }
            Ok(())
        }
    }
}

fn selected_constants(
    network: Option<&str>,
) -> Result<dg_xch_core::consensus::constants::ConsensusConstants, Error> {
    ChainSelection::from_config(network.unwrap_or("mainnet"), None)
        .and_then(|chain| chain.constants())
        .map_err(|error| Error::new(ErrorKind::InvalidInput, error))
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)]
pub async fn run_cli() -> Result<(), Error> {
    run_cli_with(Cli::parse()).await
}

#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)]
pub async fn run_cli_with(mut cli: Cli) -> Result<(), Error> {
    apply_network(&mut cli)?;
    let config_root = dg_xch_servers::app_config::config_dir(cli.config_dir.as_deref())?;
    if cli.network.is_some()
        && matches!(
            &cli.action,
            RootCommands::Gui(_)
                | RootCommands::Farmer(_)
                | RootCommands::Plotter(_)
                | RootCommands::Timelord(_)
                | RootCommands::Introducer(_)
                | RootCommands::Pool(_)
                | RootCommands::Simulator(_)
        )
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "set the network in the companion's configuration or arguments, not the launcher's root --network flag",
        ));
    }
    match &cli.action {
        RootCommands::Init(args) => {
            if cli
                .network
                .as_deref()
                .is_some_and(|network| network != "mainnet")
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "dgx init configures Chia mainnet; select other networks in explicit service configuration",
                ));
            }
            return setup::initialize(args, &config_root, cli.config_dir.is_some());
        }
        RootCommands::Gui(_) => {
            return Err(Error::other(
                "start the desktop through the dgx executable on the main thread",
            ));
        }
        RootCommands::Farmer(args) => {
            dg_xch_servers::app_config::AppConfig::load(&config_root)?;
            return services::farmer::run(&args.arguments).await;
        }
        RootCommands::Plotter(args) => {
            dg_xch_servers::app_config::AppConfig::load(&config_root)?;
            return services::plotter::run(&args.arguments);
        }
        RootCommands::Timelord(args) => {
            let worker = args
                .arguments
                .first()
                .is_some_and(|argument| argument == "worker" || argument == "regular-worker");
            if !worker {
                dg_xch_servers::app_config::AppConfig::load(&config_root)?;
            }
            #[cfg(feature = "timelord")]
            return services::timelord::run(&args.arguments).await;
            #[cfg(not(feature = "timelord"))]
            return Err(Error::other(
                "this build does not include the timelord feature",
            ));
        }
        RootCommands::Introducer(args) => {
            dg_xch_servers::app_config::AppConfig::load(&config_root)?;
            return services::introducer::run(&args.arguments).await;
        }
        RootCommands::Pool(args) => {
            dg_xch_servers::app_config::AppConfig::load(&config_root)?;
            return services::pool::run(&args.arguments).await;
        }
        RootCommands::Simulator(args) => {
            return setup::launch("dg_xch_simulator", args, &config_root).await;
        }
        _ => {}
    }
    #[cfg(feature = "full-node")]
    if let RootCommands::FullNode(args) = &mut cli.action {
        args.apply_profile(&config_root)?;
    }
    let level = env::var("RUST_LOG")
        .ok()
        .and_then(|value| value.parse::<Level>().ok())
        .unwrap_or(Level::Info);
    let _logger = DruidGardenLogger::build()
        .use_colors(true)
        .current_level(level)
        .init()
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let host = cli
        .fullnode_host
        .unwrap_or(env::var("FULLNODE_HOST").unwrap_or("localhost".to_string()));
    let initialized = config_root.join("dgx.json").try_exists()?;
    if initialized {
        dg_xch_servers::app_config::AppConfig::load(&config_root)?;
    }
    let default_port = if initialized { 8444 } else { 8555 };
    let port = cli.fullnode_port.unwrap_or(
        env::var("FULLNODE_PORT")
            .map(|s| s.parse().unwrap_or(default_port))
            .unwrap_or(default_port),
    );
    let timeout = cli.timeout.unwrap_or(60);
    let ssl = cli
        .ssl_path
        .or_else(|| initialized.then(|| config_root.join("ssl").display().to_string()))
        .map(|v| ClientSSLConfig {
            ssl_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
            ssl_key_path: format!("{}/{}", v, "full_node/private_full_node.key"),
            ssl_ca_crt_path: format!("{}/{}", v, "ca/private_ca.crt"),
        });
    match cli.action {
        RootCommands::Init(_)
        | RootCommands::Gui(_)
        | RootCommands::Farmer(_)
        | RootCommands::Plotter(_)
        | RootCommands::Timelord(_)
        | RootCommands::Introducer(_)
        | RootCommands::Pool(_)
        | RootCommands::Simulator(_) => {}
        RootCommands::Chain(args) => chain::run(args)?,
        #[cfg(feature = "full-node")]
        RootCommands::FullNode(args) => full_node::run(*args, _logger).await?,
        RootCommands::PrintPlottingInfo { launcher_id } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None)?);
            let master_key = key_from_mnemonic(&prompt_for_mnemonic()?)?;
            let mut page = 0;
            let mut plotnfts = vec![];
            if let Some(launcher_id) = launcher_id {
                info!("Searching for NFT with LauncherID: {launcher_id}");
                if let Some(plotnft) =
                    get_plotnft_by_launcher_id(client.clone(), launcher_id, None).await?
                {
                    plotnfts.push(plotnft);
                } else {
                    return Err(Error::new(
                        ErrorKind::NotFound,
                        "Failed to find a plotNFT with LauncherID: {launcher_id}",
                    ));
                }
            } else {
                info!("No LauncherID Specified, Searching for PlotNFTs...");
                while page < 50 && plotnfts.is_empty() {
                    let mut puzzle_hashes = vec![];
                    for index in page * 50..(page + 1) * 50 {
                        let wallet_sk = master_sk_to_wallet_sk_unhardened(&master_key, index)
                            .map_err(|e| {
                                Error::new(
                                    ErrorKind::InvalidInput,
                                    format!("Failed to parse Wallet SK: {e:?}"),
                                )
                            })?;
                        let pub_key: Bytes48 = wallet_sk.sk_to_pk().to_bytes().into();
                        puzzle_hashes.push(puzzle_hash_for_pk(pub_key)?);
                        let hardened_wallet_sk = master_sk_to_wallet_sk(&master_key, index)
                            .map_err(|e| {
                                Error::new(
                                    ErrorKind::InvalidInput,
                                    format!("Failed to parse Wallet SK: {e:?}"),
                                )
                            })?;
                        let pub_key: Bytes48 = hardened_wallet_sk.sk_to_pk().to_bytes().into();
                        puzzle_hashes.push(puzzle_hash_for_pk(pub_key)?);
                    }
                    plotnfts.extend(scrounge_for_plotnfts(client.clone(), &puzzle_hashes).await?);
                    page += 1;
                }
            }
            let farmer_key =
                Bytes48::from(master_sk_to_farmer_sk(&master_key)?.sk_to_pk().to_bytes());
            let pool_key = Bytes48::from(master_sk_to_pool_sk(&master_key)?.sk_to_pk().to_bytes());
            info!("{{");
            info!("\tFarmerPublicKey(All Plots): {farmer_key},");
            info!("\tPoolPublicKey(OG Plots): {pool_key},");
            info!("\tPlotNfts(NFT Plots): {{");
            let total = plotnfts.len();
            for (index, plot_nft) in plotnfts.into_iter().enumerate() {
                info!("\t  {{");
                info!("\t    LauncherID: {},", plot_nft.launcher_id);
                info!(
                    "\t    ContractAddress: {}",
                    encode_puzzle_hash(
                        &launcher_id_to_p2_puzzle_hash(
                            plot_nft.launcher_id,
                            plot_nft.delay_time as u64,
                            plot_nft.delay_puzzle_hash,
                        )?,
                        "xch"
                    )?
                );
                info!("\t  }}{}", if index == total - 1 { "" } else { "," });
            }
            info!("\t}}");
            info!("}}");
        }
        RootCommands::GetBlockchainState => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_blockchain_state().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    println!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlock { header_hash } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_block(&header_hash).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlockCountMetrics => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_block_count_metrics().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlocks {
            start,
            end,
            exclude_header_hash,
            exclude_reorged,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_blocks(start, end, exclude_header_hash, exclude_reorged)
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetAllBlocks { start, end } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_all_blocks(start, end).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlockRecord { header_hash } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_block_record(&header_hash).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlockRecordByHeight { height } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_block_record_by_height(height).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetBlockRecords { start, end } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_block_records(start, end).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetUnfinishedBlocks => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_unfinished_block_headers().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetNetworkSpace {
            older_block_header_hash,
            newer_block_header_hash,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_network_space(&older_block_header_hash, &newer_block_header_hash)
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetNetworkSpaceaByHeight { start, end } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_network_space_by_height(start, end).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetAdditionsAndRemovals { header_hash } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_additions_and_removals(&header_hash).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetNetworkInfo => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_network_info().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetSignagePointOrEOS {
            sp_hash,
            challenge_hash,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_recent_signage_point_or_eos(sp_hash.as_ref(), challenge_hash.as_ref())
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecords {
            puzzle_hashes,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_puzzle_hashes(
                    &puzzle_hashes,
                    include_spent_coins,
                    start_height,
                    end_height,
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordByName { name } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_coin_record_by_name(&name).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByNames {
            names,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_names(
                    &names,
                    Some(include_spent_coins),
                    Some(start_height),
                    Some(end_height),
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByParentIds {
            parent_ids,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_parent_ids(
                    &parent_ids,
                    Some(include_spent_coins),
                    Some(start_height),
                    Some(end_height),
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByhint {
            hint,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_hint(
                    &hint,
                    Some(include_spent_coins),
                    Some(start_height),
                    Some(end_height),
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetPuzzleAndSolution { coin_id, height } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_puzzle_and_solution(&coin_id, height).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinSpend { coin_id, height } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_puzzle_and_solution(&coin_id, height).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetAllMempoolTxIds => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_all_mempool_tx_ids().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetAllMempoolItems => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_all_mempool_items().await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetMempoolItemByTxID { tx_id } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_mempool_item_by_tx_id(&tx_id).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetMempoolItemByName { coin_name } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_mempool_items_by_coin_name(&coin_name).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetFeeEstimate {
            cost,
            spend_bundle,
            spend_type,
            target_times,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_fee_estimate(
                    cost,
                    spend_bundle.map(|s| {
                        if s.starts_with("0x") {
                            let decoded = decode(s).expect("String is not valid SpendBundle hex");
                            let mut cur = Cursor::new(decoded.as_slice());
                            SpendBundle::from_bytes(&mut cur, ChiaProtocolVersion::default())
                                .expect("String is not valid SpendBundle Hex")
                        } else {
                            serde_json::from_str(&s).expect("String is not a valid SpendBundle")
                        }
                    }),
                    spend_type,
                    &target_times,
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        //End Fullnode API, Start of Extended Fullnode API
        RootCommands::GetSingletonByLauncherId { launcher_id } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_singleton_by_launcher_id(&launcher_id).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetAdditionsAndRemovalsWithHints { header_hash } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_additions_and_removals_with_hints(&header_hash)
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByHints {
            hints,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_hints(
                    &hints,
                    Some(include_spent_coins),
                    Some(start_height),
                    Some(end_height),
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByHintsPaginated {
            hints,
            include_spent_coins,
            start_height,
            end_height,
            page_size,
            last_id,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_hints_paginated(
                    &hints,
                    include_spent_coins,
                    start_height,
                    end_height,
                    page_size,
                    last_id,
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetCoinRecordsByPuzzleHashesPaginated {
            puzzle_hashes,
            include_spent_coins,
            start_height,
            end_height,
            page_size,
            last_id,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_coin_records_by_puzzle_hashes_paginated(
                    &puzzle_hashes,
                    include_spent_coins,
                    start_height,
                    end_height,
                    page_size,
                    last_id,
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetHintsByCoinIds { coin_ids } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_hints_by_coin_ids(&coin_ids).await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        RootCommands::GetPuzzleAndSoultionsByNames {
            names,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client
                .get_puzzles_and_solutions_by_names(
                    &names,
                    include_spent_coins,
                    start_height,
                    end_height,
                )
                .await?;
            match serde_json::to_string_pretty(&results) {
                Ok(json) => {
                    info!("{json}");
                }
                Err(e) => {
                    error!("Failed to convert value to JSON: {e:?}");
                }
            }
        }
        //End Extended Fullnode API
        RootCommands::MovePlotNFT {
            target_pool,
            launcher_id,
            target_address,
            mnemonic,
            fee,
        } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None)?);
            migrate_plot_nft(
                client,
                &target_pool,
                launcher_id,
                target_address,
                &mnemonic,
                Arc::new(selected_constants(cli.network.as_deref())?),
                fee.unwrap_or_default(),
            )
            .await?;
        }
        RootCommands::MovePlotNFTWithOwnerKey {
            target_pool,
            launcher_id,
            target_address,
            owner_key,
        } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None)?);
            let owner_key = SecretKey::from_bytes(Bytes32::from_str(&owner_key)?.as_ref())
                .expect("Failed to Parse Owner Secret Key");
            migrate_plot_nft_with_owner_key(
                client,
                &target_pool,
                launcher_id,
                target_address,
                &owner_key,
            )
            .await?;
        }
        RootCommands::GetPlotnftState { launcher_id } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None)?);
            get_plotnft_ready_state(client, launcher_id, None)
                .await
                .map(|_| ())?;
        }
        RootCommands::CreatePoolLoginLink {
            target_pool,
            launcher_id,
            auth_key,
        } => {
            let url =
                create_pool_login_url(&target_pool, &[(auth_key.into(), launcher_id)]).await?;
            println!("{url}");
        }
        RootCommands::CreateWallet { action } => match action {
            WalletAction::WithNFT { .. } => {}
            WalletAction::Cold => create_cold_wallet(&selected_constants(cli.network.as_deref())?)?,
        },
        RootCommands::Curry {
            program,
            args,
            output,
        } => {
            let prog_as_path = Path::new(&program);
            let args_as_path = Path::new(&args);
            let serial_program;
            let serial_args;
            let program = if prog_as_path.exists() {
                Program::from_file(prog_as_path).await?
            } else if is_hex(program.as_bytes()) {
                serial_program = SerializedProgram::from_bytes(program.as_bytes());
                Program::from_serial(&serial_program)?
            } else {
                assemble_text(&program)?
            };
            let args = if args_as_path.exists() {
                Program::from_file(args_as_path).await?
            } else if is_hex(args.as_bytes()) {
                serial_args = SerializedProgram::from_bytes(args.as_bytes());
                Program::from_serial(&serial_args)?
            } else {
                assemble_text(&args)?
            };
            let arg_list = args.as_list();
            let curried_program = program.curry(&arg_list);
            match output.unwrap_or_default() {
                ProgramOutput::Hex => {
                    println!("{}", encode(&sexp_to_bytes(curried_program.sexp())?))
                }
                ProgramOutput::String => {
                    println!("{curried_program}")
                }
            }
        }
        RootCommands::Run {
            program,
            args,
            output,
        } => {
            let prog_as_path = Path::new(&program);
            let asrg_as_path = Path::new(&args);
            let serial_program;
            let serial_args;
            let program = if prog_as_path.exists() {
                Program::from_file(prog_as_path).await?
            } else if is_hex(program.as_bytes()) {
                serial_program = SerializedProgram::from_bytes(program.as_bytes());
                Program::from_serial(&serial_program)?
            } else {
                assemble_text(&program)?
            };
            let args = if asrg_as_path.exists() {
                Program::from_file(asrg_as_path).await?
            } else if is_hex(args.as_bytes()) {
                serial_args = SerializedProgram::from_bytes(args.as_bytes());
                Program::from_serial(&serial_args)?
            } else {
                assemble_text(&args)?
            };
            let (_cost, program_output) = program.run(INFINITE_COST, 0, &args)?;
            match output.unwrap_or_default() {
                ProgramOutput::Hex => {
                    println!("{}", encode(&sexp_to_bytes(program_output.sexp())?))
                }
                ProgramOutput::String => {
                    println!("{program_output}")
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod network_tests {
    use super::*;

    #[test]
    fn inherited_network_must_agree_with_an_explicit_subcommand_network() {
        let mut network = None;
        inherit_network(&mut network, Some("testnet11")).unwrap();
        assert_eq!(network.as_deref(), Some("testnet11"));
        inherit_network(&mut network, None).unwrap();
        inherit_network(&mut network, Some("testnet11")).unwrap();
        assert!(inherit_network(&mut network, Some("mainnet")).is_err());
        assert_eq!(network.as_deref(), Some("testnet11"));
    }

    #[test]
    fn commands_without_a_chain_manifest_reject_unknown_explicit_networks() {
        let mut cli =
            Cli::try_parse_from(["dgx", "--network", "not-a-network", "get-network-info"]).unwrap();
        assert!(apply_network(&mut cli).is_err());
        let mut cli = Cli::try_parse_from(["dgx", "get-network-info"]).unwrap();
        apply_network(&mut cli).unwrap();
        assert_eq!(
            selected_constants(None).unwrap(),
            dg_xch_core::consensus::constants::MAINNET
        );
    }
}
