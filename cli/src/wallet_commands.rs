use bip39::Mnemonic;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_keys::*;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    calculate_synthetic_secret_key, puzzle_hash_for_pk, DEFAULT_HIDDEN_PUZZLE_HASH,
};
use log::{error, info};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::wallets::plotnft_utils::{PlotNFTWallet, scrounge_for_plotnft_by_key};
use crate::wallets::{Wallet, WalletInfo};
use crate::wallets::memory_wallet::{MemoryWalletConfig, MemoryWalletStore};
use dg_xch_clients::api::pool::{DefaultPoolClient, PoolClient};
use dg_xch_clients::protocols::pool::{FARMING_TO_POOL, POOL_PROTOCOL_VERSION};
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::wallet_type::WalletType;
use dg_xch_core::consensus::constants::MAINNET;
use dg_xch_core::pool::PoolState;

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
    client: FullnodeClient,
    target_pool: String,
    launcher_id: String,
    mnemonic: String,
) -> Result<(), Error> {
    let pool_client = DefaultPoolClient::new();
    let master_secret_key = key_from_mnemonic(&mnemonic)?;
    let wallet_sk = master_sk_to_wallet_sk_unhardened(&master_secret_key, 1).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Failed to parse Wallet SK: {:?}", e),
        )
    })?;
    let pub_key: Bytes48 = wallet_sk.sk_to_pk().to_bytes().into();
    let starting_ph = puzzle_hash_for_pk(&pub_key)?;
    info!("{}", encode_puzzle_hash(&starting_ph, "xch").unwrap());
    let plot_nfts = scrounge_for_plotnft_by_key(&client, &master_secret_key).await?;
    info!("Found {} plot_nfts", plot_nfts.len());
    let pool_url = format!("https://{}", target_pool);
    let pool_info = pool_client.get_pool_info(&pool_url).await.map_err(|e| {
        Error::new(
            ErrorKind::Other,
            format!("Failed to load pool info: {:?}", e),
        )
    })?;
    if pool_info.relative_lock_height > 1000 {
        let error_message = "Relative lock height too high for this pool, cannot join";
        error!("{}", error_message);
        return Err(Error::new(ErrorKind::InvalidData, error_message))
    }
    if pool_info.protocol_version != POOL_PROTOCOL_VERSION {
        let error_message = format!("Incorrect version: {}, should be {POOL_PROTOCOL_VERSION}", pool_info.protocol_version);
        error!("{}", error_message);
        return Err(Error::new(ErrorKind::InvalidData, error_message))
    }
    let pool_wallet = PlotNFTWallet::create(
        WalletInfo {
            id: 1,
            name: "pooling_wallet".to_string(),
            wallet_type: WalletType::PoolingWallet,
            constants: Default::default(),
            master_sk: master_secret_key.clone(),
            wallet_store: Arc::new(Mutex::new(MemoryWalletStore::new(
                master_secret_key,
                0
            ))),
            data: "".to_string(),
        },
        MemoryWalletConfig {
            fullnode_host: client.host.clone(),
            fullnode_port: client.port,
            fullnode_ssl_path: client.ssl_path.clone(),
            additional_headers: client.additional_headers.clone(),
        }
    );
    let launcher_to_find = Bytes32::from(launcher_id);
    if !pool_wallet.sync().await? {
        error!("Failed to Sync Wallet");
        return Err(Error::new(ErrorKind::Other, "Failed to Sync"));
    }
    for plot_nft in plot_nfts {
        if plot_nft.launcher_id != launcher_to_find {
            continue;
        } else {
            info!("Found Launcher ID to Migrate");
        }
        let owner_sk = pool_wallet.find_owner_key(&plot_nft.pool_state.owner_pubkey, 500)?;
        let target_pool_state = PoolState {
            owner_pubkey: Bytes48::from_sized_bytes(owner_sk.sk_to_pk().to_bytes()),
            pool_url: Some(pool_url.to_string()),
            relative_lock_height: pool_info.relative_lock_height,
            state: FARMING_TO_POOL,  //# Farming to Pool
            target_puzzle_hash: pool_info.target_puzzle_hash,
            version: 1,
        };
        if plot_nft.pool_state == target_pool_state {
            let error_message = format!("Current State equal to Target State: {:?}", &target_pool_state);
            error!("{}", error_message);
            return Err(Error::new(ErrorKind::InvalidData, error_message))
        }
        let fee = 40;
        let (travel_record, fee_record) = pool_wallet.generate_travel_transaction(
            &plot_nft,
            &target_pool_state,
            fee,
            &MAINNET
        ).await?;
        info!("{:?}", travel_record);
        info!("{:?}", fee_record);
        break;
    }
    Ok(())
}