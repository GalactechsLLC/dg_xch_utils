use crate::cli::ProgramOutput;
use crate::wallet_commands::{
    create_cold_wallet, get_plotnft_ready_state, migrate_plot_nft, migrate_plot_nft_with_owner_key,
};
use crate::wallets::plotnft_utils::{get_plotnft_by_launcher_id, scrounge_for_plotnfts};
use blst::min_pk::SecretKey;
use clap::Parser;
use cli::{prompt_for_mnemonic, Cli, RootCommands, WalletAction};
use dg_logger::DruidGardenLogger;
use dg_xch_clients::api::full_node::{FullnodeAPI, FullnodeExtAPI};
use dg_xch_clients::api::pool::create_pool_login_url;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::clvm::assemble::{assemble_text, is_hex};
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::clvm::utils::INFINITE_COST;
use dg_xch_core::consensus::constants::{CONSENSUS_CONSTANTS_MAP, MAINNET};
use dg_xch_keys::{
    encode_puzzle_hash, key_from_mnemonic, master_sk_to_farmer_sk, master_sk_to_pool_sk,
    master_sk_to_wallet_sk, master_sk_to_wallet_sk_unhardened,
};
use dg_xch_puzzles::clvm_puzzles::launcher_id_to_p2_puzzle_hash;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use hex::{decode, encode};
use log::{error, info, Level};
use std::env;
use std::io::{Cursor, Error, ErrorKind};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

pub mod cli;
pub mod commands;
pub mod simulator;
pub mod wallet_commands;
pub mod wallets;

#[allow(clippy::too_many_lines)]
#[allow(clippy::cast_sign_loss)]
pub async fn run_cli() -> Result<(), Error> {
    let cli = Cli::parse();
    let _logger = DruidGardenLogger::build()
        .use_colors(true)
        .current_level(Level::Info)
        .init()
        .map_err(|e| Error::other(format!("{e:?}")))?;
    let host = cli
        .fullnode_host
        .unwrap_or(env::var("FULLNODE_HOST").unwrap_or("localhost".to_string()));
    let port = cli.fullnode_port.unwrap_or(
        env::var("FULLNODE_PORT")
            .map(|s| s.parse().unwrap_or(8555))
            .unwrap_or(8555),
    );
    let timeout = cli.timeout.unwrap_or(60);
    let ssl = cli.ssl_path.map(|v| ClientSSLConfig {
        ssl_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
        ssl_key_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
        ssl_ca_crt_path: format!("{}/{}", v, "full_node/private_full_node.crt"),
    });
    let constants = if let Some(network) = cli.network {
        CONSENSUS_CONSTANTS_MAP
            .get(&network)
            .cloned()
            .unwrap_or_else(|| MAINNET.clone())
    } else {
        MAINNET.clone()
    };
    match cli.action {
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
            info!("\tFarmerPublicKey(All Plots): {},", farmer_key);
            info!("\tPoolPublicKey(OG Plots): {},", pool_key);
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
        RootCommands::GetInitialFreezePeriod => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None)?;
            let results = client.get_initial_freeze_period().await?;
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
                            let mut cur = Cursor::new(
                                decode(s).expect("String is not valid SpendBundle Hex"),
                            );
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
                constants.clone(),
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
            WalletAction::Cold => create_cold_wallet()?,
        },
        RootCommands::Curry {
            program,
            args,
            output,
        } => {
            let prog_as_path = Path::new(&program);
            let asrg_as_path = Path::new(&args);
            let program = if prog_as_path.exists() {
                SerializedProgram::from_file(prog_as_path)
                    .await?
                    .to_program()
            } else if is_hex(program.as_bytes()) {
                SerializedProgram::from_bytes(program.as_bytes()).to_program()
            } else {
                assemble_text(&program)?.to_program()
            };
            let args = if asrg_as_path.exists() {
                SerializedProgram::from_file(asrg_as_path)
                    .await?
                    .to_program()
            } else if is_hex(args.as_bytes()) {
                SerializedProgram::from_bytes(args.as_bytes()).to_program()
            } else {
                assemble_text(&args)?.to_program()
            };
            let curried_program = program.curry(&args.as_list())?;
            match output.unwrap_or_default() {
                ProgramOutput::Hex => {
                    println!("{}", encode(&curried_program.serialized))
                }
                ProgramOutput::String => {
                    println!("{}", curried_program)
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
            let program = if prog_as_path.exists() {
                SerializedProgram::from_file(prog_as_path)
                    .await?
                    .to_program()
            } else if is_hex(program.as_bytes()) {
                SerializedProgram::from_bytes(program.as_bytes()).to_program()
            } else {
                assemble_text(&program)?.to_program()
            };
            let args = if asrg_as_path.exists() {
                SerializedProgram::from_file(asrg_as_path)
                    .await?
                    .to_program()
            } else if is_hex(args.as_bytes()) {
                SerializedProgram::from_bytes(args.as_bytes()).to_program()
            } else {
                assemble_text(&args)?.to_program()
            };
            let (_cost, program_output) = program.run(INFINITE_COST, 0, &args)?;
            match output.unwrap_or_default() {
                ProgramOutput::Hex => {
                    println!("{}", encode(&program_output.serialized))
                }
                ProgramOutput::String => {
                    println!("{}", program_output)
                }
            }
        }
    }
    Ok(())
}
