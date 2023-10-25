use crate::wallets::plotnft_utils::{
    get_plotnft_by_launcher_id, submit_next_state_spend_bundle,
    submit_next_state_spend_bundle_with_key, PlotNFTWallet,
};
use crate::wallets::Wallet;
use bip39::Mnemonic;
use blst::min_pk::SecretKey;
use dg_xch_clients::api::full_node::FullnodeAPI;
use dg_xch_clients::api::pool::{DefaultPoolClient, PoolClient};
use dg_xch_clients::protocols::pool::{
    GetPoolInfoResponse, FARMING_TO_POOL, POOL_PROTOCOL_VERSION,
};
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::plots::PlotNft;
use dg_xch_core::pool::PoolState;
use dg_xch_keys::*;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    calculate_synthetic_secret_key, puzzle_hash_for_pk, DEFAULT_HIDDEN_PUZZLE_HASH,
};
use log::{debug, error, info};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::ops::Add;
use std::time::{Duration, Instant};

pub fn create_cold_wallet() -> Result<(), Error> {
    let mnemonic = Mnemonic::generate(24)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
    let master_secret_key = key_from_mnemonic(&mnemonic.to_string())?;
    let master_public_key = master_secret_key.sk_to_pk();
    let fp = fingerprint(&master_public_key);
    info!("Fingerprint: {fp}");
    info!("Mnemonic Phrase: {}", &mnemonic.to_string());
    info!(
        "Master public key (m): {}",
        Bytes48::from(master_public_key.to_bytes())
    );
    info!(
        "Farmer public key (m/{BLS_SPEC_NUMBER}/{CHIA_BLOCKCHAIN_NUMBER}/{FARMER_PATH}/0): {}",
        Bytes48::from(
            master_sk_to_farmer_sk(&master_secret_key)?
                .sk_to_pk()
                .to_bytes()
        )
    );
    info!(
        "Pool public key (m/{BLS_SPEC_NUMBER}/{CHIA_BLOCKCHAIN_NUMBER}/{POOL_PATH}/0: {}",
        Bytes48::from(
            master_sk_to_pool_sk(&master_secret_key)?
                .sk_to_pk()
                .to_bytes()
        )
    );
    info!("First 3 Wallet addresses");
    for i in 0..3 {
        let wallet_sk = master_sk_to_wallet_sk(&master_secret_key, i)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?;
        let address = encode_puzzle_hash(
            &puzzle_hash_for_pk(&Bytes48::from(wallet_sk.sk_to_pk().to_bytes()))?,
            "xch",
        )?;
        info!("Index: {}, Address: {}", i, address);
    }
    Ok(())
}

pub fn keys_for_coinspends(
    coin_spends: &[CoinSpend],
    master_sk: &SecretKey,
    max_pub_keys: u32,
) -> Result<HashMap<Bytes48, SecretKey>, Error> {
    let mut key_cache: HashMap<Bytes48, SecretKey> = HashMap::new();
    let mut puz_key_cache: HashSet<Bytes32> = HashSet::new();
    let mut last_key_index = 0;
    for c in coin_spends {
        if puz_key_cache.contains(&c.coin.puzzle_hash) {
            continue;
        } else {
            for ki in last_key_index..max_pub_keys {
                let sec_key = master_sk_to_wallet_sk(master_sk, ki)?;
                let pub_key = sec_key.sk_to_pk();
                let puz_hash = puzzle_hash_for_pk(&pub_key.into())?;
                let synthetic_secret_key =
                    calculate_synthetic_secret_key(&sec_key, &DEFAULT_HIDDEN_PUZZLE_HASH)?;
                info!("MasterSK: {:?}", master_sk);
                info!("WalletSK: {:?}", sec_key);
                info!("SyntheticSK: {:?}", synthetic_secret_key);
                key_cache.insert(pub_key.into(), synthetic_secret_key.clone());
                puz_key_cache.insert(puz_hash);
                if c.coin.puzzle_hash == puz_hash {
                    last_key_index = ki;
                    break;
                }
            }
        }
    }
    Ok(key_cache)
}

pub async fn migrate_plot_nft(
    client: &FullnodeClient,
    target_pool: &str,
    launcher_id: &Bytes32,
    mnemonic: &str,
    fee: u64,
) -> Result<(), Error> {
    let pool_url = if target_pool.starts_with("https://") {
        target_pool.to_string()
    } else {
        format!("https://{}", target_pool)
    };
    let pool_info = get_pool_info(&pool_url).await?;
    let pool_wallet = PlotNFTWallet::new(key_from_mnemonic(mnemonic)?, client);
    info!("Searching for PlotNFT with LauncherID: {launcher_id}");
    if let Some(mut plot_nft) = get_plotnft_by_launcher_id(client, launcher_id).await? {
        info!("Checking if PlotNFT needs migration");
        if plot_nft.pool_state.pool_url.as_ref() != Some(&pool_url) {
            info!("Starting Migration");
            let target_pool_state =
                create_and_validate_target_state(&pool_url, pool_info, &plot_nft)?;
            if plot_nft.pool_state.state == FARMING_TO_POOL {
                info!("Creating Leaving Pool Spend");
                if fee > 0 && !pool_wallet.sync().await? {
                    error!("Failed to Sync Wallet");
                    return Err(Error::new(ErrorKind::Other, "Failed to Sync"));
                }
                submit_next_state_spend_bundle(
                    client,
                    &pool_wallet,
                    &plot_nft,
                    &target_pool_state,
                    fee,
                )
                .await?;
                info!(
                    "Waiting for PlotNFT State to be Buried for Leaving {}",
                    plot_nft
                        .pool_state
                        .pool_url
                        .as_ref()
                        .unwrap_or(&String::from("None"))
                );
                wait_for_plot_nft_ready_state(client, launcher_id).await;
                info!("Reloading PlotNFT Info");
                plot_nft = get_plotnft_by_launcher_id(client, launcher_id)
                    .await?
                    .ok_or_else(|| {
                        error!("Failed to reload plot_nft after first spend");
                        Error::new(
                            ErrorKind::Other,
                            "Failed to reload plot_nft after first spend",
                        )
                    })?;
            }
            info!("Creating Farming to Pool Spend");
            if fee > 0 && !pool_wallet.sync().await? {
                error!("Failed to Sync Wallet");
                return Err(Error::new(ErrorKind::Other, "Failed to Sync"));
            }
            submit_next_state_spend_bundle(
                client,
                &pool_wallet,
                &plot_nft,
                &target_pool_state,
                fee,
            )
            .await?;
            info!("Waiting for PlotNFT State to be Buried for Joining {pool_url}");
            wait_for_plot_nft_ready_state(client, launcher_id).await;
        } else {
            info!("PlotNFT Already on Selected Pool");
        }
    } else {
        info!("No PlotNFT Found");
    }
    Ok(())
}
pub async fn migrate_plot_nft_with_owner_key(
    client: &FullnodeClient,
    target_pool: &str,
    launcher_id: &Bytes32,
    owner_key: &SecretKey,
) -> Result<(), Error> {
    let pool_url = if target_pool.starts_with("https://") {
        target_pool.to_string()
    } else {
        format!("https://{}", target_pool)
    };
    let pool_info = get_pool_info(&pool_url).await?;
    info!("Searching for PlotNFT with LauncherID: {launcher_id}");
    if let Some(mut plot_nft) = get_plotnft_by_launcher_id(client, launcher_id).await? {
        info!("Checking if PlotNFT needs migration");
        if plot_nft.pool_state.pool_url.as_ref() != Some(&pool_url) {
            info!("Starting Migration");
            let target_pool_state =
                create_and_validate_target_state(&pool_url, pool_info, &plot_nft)?;
            if plot_nft.pool_state.state == FARMING_TO_POOL {
                info!("Creating Leaving Pool Spend");
                submit_next_state_spend_bundle_with_key(
                    client,
                    owner_key,
                    &plot_nft,
                    &target_pool_state,
                    &Default::default(),
                )
                .await?;
                info!(
                    "Waiting for PlotNFT State to be Buried for Leaving {}",
                    plot_nft
                        .pool_state
                        .pool_url
                        .as_ref()
                        .unwrap_or(&String::from("None"))
                );
                wait_for_plot_nft_ready_state(client, launcher_id).await;
                info!("Reloading PlotNFT Info");
                plot_nft = get_plotnft_by_launcher_id(client, launcher_id)
                    .await?
                    .ok_or_else(|| {
                        error!("Failed to reload plot_nft after first spend");
                        Error::new(
                            ErrorKind::Other,
                            "Failed to reload plot_nft after first spend",
                        )
                    })?;
            }
            info!("Creating Farming to Pool Spend");
            submit_next_state_spend_bundle_with_key(
                client,
                owner_key,
                &plot_nft,
                &target_pool_state,
                &Default::default(),
            )
            .await?;
            info!("Waiting for PlotNFT State to be Buried for Joining {pool_url}");
            wait_for_num_blocks(client, 20, 600).await;
        } else {
            info!("PlotNFT Already on Selected Pool");
        }
    } else {
        info!("No PlotNFT Found");
    }
    Ok(())
}

async fn wait_for_plot_nft_ready_state(client: &FullnodeClient, launcher_id: &Bytes32) {
    loop {
        match get_plotnft_ready_state(client, launcher_id).await {
            Ok(is_ready) => {
                if is_ready {
                    break;
                } else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
            Err(e) => {
                error!(
                    "Error Checking PlotNFT State, Waiting and Trying again. {:?}",
                    e
                );
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

async fn wait_for_num_blocks(client: &FullnodeClient, height: u32, timeout_seconds: u64) {
    let mut start_height = None;
    let end_time = Instant::now().add(Duration::from_secs(timeout_seconds));
    loop {
        let now = Instant::now();
        if now >= end_time {
            break;
        }
        match client.get_blockchain_state().await {
            Ok(state) => {
                if let Some(peak) = state.peak {
                    if let Some(start) = start_height {
                        if peak.height > start + height {
                            break;
                        } else {
                            info!("Waiting for {} more blocks", start + height - peak.height);
                            tokio::time::sleep(std::cmp::min(
                                end_time.duration_since(now),
                                Duration::from_secs(10),
                            ))
                            .await;
                        }
                    } else {
                        start_height = Some(peak.height);
                    }
                }
            }
            Err(e) => {
                error!(
                    "Error Checking PlotNFT State, Waiting and Trying again. {:?}",
                    e
                );
            }
        }
    }
}

fn create_and_validate_target_state(
    pool_url: &str,
    pool_info: GetPoolInfoResponse,
    plot_nft: &PlotNft,
) -> Result<PoolState, Error> {
    let target_pool_state = PoolState {
        owner_pubkey: plot_nft.pool_state.owner_pubkey,
        pool_url: Some(pool_url.to_string()),
        relative_lock_height: pool_info.relative_lock_height,
        state: FARMING_TO_POOL, //# Farming to Pool
        target_puzzle_hash: pool_info.target_puzzle_hash,
        version: 1,
    };
    if plot_nft.pool_state == target_pool_state {
        let error_message = format!(
            "Current State equal to Target State: {:?}",
            &target_pool_state
        );
        error!("{}", error_message);
        return Err(Error::new(ErrorKind::InvalidData, error_message));
    }
    info!(
        "Targeting State: {}",
        serde_json::to_string_pretty(&target_pool_state).unwrap_or_default()
    );
    Ok(target_pool_state)
}

async fn get_pool_info(pool_url: &str) -> Result<GetPoolInfoResponse, Error> {
    let pool_info = DefaultPoolClient::new()
        .get_pool_info(pool_url)
        .await
        .map_err(|e| {
            Error::new(
                ErrorKind::Other,
                format!("Failed to load pool info: {:?}", e),
            )
        })?;
    validate_pool_info(&pool_info)?;
    Ok(pool_info)
}

fn validate_pool_info(pool_info: &GetPoolInfoResponse) -> Result<(), Error> {
    if pool_info.relative_lock_height > 1000 {
        let error_message = "Relative lock height too high for this pool, cannot join";
        error!("{}", error_message);
        Err(Error::new(ErrorKind::InvalidData, error_message))
    } else if pool_info.protocol_version != POOL_PROTOCOL_VERSION {
        let error_message = format!(
            "Incorrect version: {}, should be {POOL_PROTOCOL_VERSION}",
            pool_info.protocol_version
        );
        error!("{}", error_message);
        Err(Error::new(ErrorKind::InvalidData, error_message))
    } else {
        Ok(())
    }
}

pub async fn get_plotnft_ready_state(
    client: &FullnodeClient,
    launcher_id: &Bytes32,
) -> Result<bool, Error> {
    let mut peak = None;
    while peak.is_none() {
        peak = client.get_blockchain_state().await?.peak;
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    let peak = peak.unwrap();
    match get_plotnft_by_launcher_id(client, launcher_id).await? {
        None => {
            error!("Failed to find PlotNFT with LauncherID: {}", launcher_id);
            Ok(false)
        }
        Some(plotnft) => {
            debug!("Found PlotNFT: {}", plotnft.launcher_id);
            let test_height = plotnft.pool_state.relative_lock_height
                + 2
                + plotnft.singleton_coin.confirmed_block_index;
            info!(
                "Ready to move {}: {}, current_height: {}, target_height {}",
                plotnft.launcher_id,
                peak.height >= test_height,
                peak.height,
                test_height
            );
            Ok(peak.height >= test_height)
        }
    }
}
