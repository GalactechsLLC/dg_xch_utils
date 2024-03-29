pub mod cli;

use blst::min_pk::SecretKey;
use clap::Parser;
use cli::*;
use dg_xch_cli::wallet_commands::{
    create_cold_wallet, get_plotnft_ready_state, migrate_plot_nft, migrate_plot_nft_with_owner_key,
};
use dg_xch_clients::api::full_node::{FullnodeAPI, FullnodeExtAPI};
use dg_xch_clients::api::pool::create_pool_login_url;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_serialize::ChiaSerialize;
use hex::decode;
use log::{error, info, LevelFilter};
use simple_logger::SimpleLogger;
use std::env;
use std::io::{Cursor, Error};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = Cli::parse();
    SimpleLogger::new()
        .with_level(LevelFilter::Info)
        .with_colors(true)
        .env()
        .init()
        .unwrap_or_default();
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
    match cli.action {
        RootCommands::GetBlockchainState => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
            let results = client
                .get_coin_records_by_names(&names, include_spent_coins, start_height, end_height)
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
            let results = client
                .get_coin_records_by_parent_ids(
                    &parent_ids,
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
        RootCommands::GetCoinRecordsByhint {
            hint,
            include_spent_coins,
            start_height,
            end_height,
        } => {
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
            let results = client
                .get_coin_records_by_hint(&hint, include_spent_coins, start_height, end_height)
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
            let results = client
                .get_fee_estimate(
                    cost,
                    spend_bundle.map(|s| {
                        if s.starts_with("0x") {
                            let mut cur = Cursor::new(
                                decode(s).expect("String is not valid SpendBundle Hex"),
                            );
                            SpendBundle::from_bytes(&mut cur)
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
            let results = client
                .get_coin_records_by_hints(&hints, include_spent_coins, start_height, end_height)
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            let client = FullnodeClient::new(&host, port, timeout, ssl, &None);
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
            mnemonic,
            fee,
        } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None));
            migrate_plot_nft(
                client,
                &target_pool,
                &launcher_id,
                &mnemonic,
                fee.unwrap_or_default(),
            )
            .await?
        }
        RootCommands::MovePlotNFTWithOwnerKey {
            target_pool,
            launcher_id,
            owner_key,
        } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None));
            let owner_key = SecretKey::from_bytes(Bytes32::from(&owner_key).as_ref())
                .expect("Failed to Parse Owner Secret Key");
            migrate_plot_nft_with_owner_key(client, &target_pool, &launcher_id, &owner_key).await?
        }
        RootCommands::GetPlotnftState { launcher_id } => {
            let client = Arc::new(FullnodeClient::new(&host, port, timeout, ssl, &None));
            get_plotnft_ready_state(client, &launcher_id)
                .await
                .map(|_| ())?
        }
        RootCommands::CreatePoolLoginLink {
            target_pool,
            launcher_id,
            auth_key,
        } => {
            let url =
                create_pool_login_url(&target_pool, &[(auth_key.into(), launcher_id)]).await?;
            println!("{}", url);
        }
        RootCommands::CreateWallet { action } => match action {
            WalletAction::WithNFT { .. } => {}
            WalletAction::Cold => create_cold_wallet()?,
        },
    }
    Ok(())
}
