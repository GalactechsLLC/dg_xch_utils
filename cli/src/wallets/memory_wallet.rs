use crate::wallets::common::DerivationRecord;
use crate::wallets::{SecretKeyStore, Wallet, WalletInfo, WalletStore};
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dashmap::DashMap;
use dg_xch_clients::api::full_node::{FullnodeAPI, FullnodeExtAPI};
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_core::blockchain::coin_record::{CatCoinRecord, CatVersion, CoinRecord};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::sexp::IntoSExp;
use dg_xch_puzzles::cats::{CAT_1_PROGRAM, CAT_2_PROGRAM};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    calculate_synthetic_secret_key, DEFAULT_HIDDEN_PUZZLE_HASH,
};
use log::{error, info};
use std::collections::HashMap;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct MemoryWalletConfig {
    pub fullnode_host: String,
    pub fullnode_port: u16,
    pub fullnode_ssl_path: Option<ClientSSLConfig>,
    pub additional_headers: Option<HashMap<String, String>>,
}

pub struct MemoryWalletStore {
    pub master_sk: SecretKey,
    pub current_index: AtomicU32,
    standard_coins: Arc<Mutex<Vec<CoinRecord>>>,
    cat_coins: Arc<Mutex<Vec<CatCoinRecord>>>,
    derivation_records: DashMap<Bytes32, DerivationRecord>,
    keys_for_ph: DashMap<Bytes32, (Bytes32, Bytes48)>,
    secret_key_store: SecretKeyStore,
}
impl MemoryWalletStore {
    pub fn new(secret_key: SecretKey, starting_index: u32) -> Self {
        Self {
            master_sk: secret_key,
            current_index: AtomicU32::new(starting_index),
            standard_coins: Default::default(),
            cat_coins: Default::default(),
            keys_for_ph: Default::default(),
            derivation_records: Default::default(),
            secret_key_store: Default::default(),
        }
    }
}
#[async_trait]
impl WalletStore for MemoryWalletStore {
    fn get_master_sk(&self) -> &SecretKey {
        &self.master_sk
    }

    fn standard_coins(&self) -> Arc<Mutex<Vec<CoinRecord>>> {
        self.standard_coins.clone()
    }

    fn cat_coins(&self) -> Arc<Mutex<Vec<CatCoinRecord>>> {
        self.cat_coins.clone()
    }

    fn secret_key_store(&self) -> &SecretKeyStore {
        &self.secret_key_store
    }

    fn current_index(&self) -> u32 {
        self.current_index.load(Ordering::Relaxed)
    }

    fn next_index(&self) -> u32 {
        self.current_index.fetch_add(1, Ordering::Relaxed)
    }

    async fn get_confirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_unconfirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_pending_change_balance(&self) -> u128 {
        todo!()
    }

    async fn populate_secret_key_for_puzzle_hash(
        &self,
        puz_hash: &Bytes32,
    ) -> Result<Bytes48, Error> {
        if self.keys_for_ph.is_empty() || self.keys_for_ph.get(puz_hash).is_none() {
            info!("Populating Initial PuzzleHashes");
            for i in self.current_index.load(Ordering::Relaxed)
                ..=(self.current_index.load(Ordering::Relaxed) + 100)
            {
                let hardened_record = self.get_derivation_record_at_index(i, true).await?;
                self.derivation_records
                    .insert(hardened_record.puzzle_hash, hardened_record);
                let record = self.get_derivation_record_at_index(i, false).await?;
                self.derivation_records.insert(record.puzzle_hash, record);
            }
        }
        match self.keys_for_ph.get(puz_hash) {
            None => {
                error!("Failed to find keys for puzzle hash");
                Err(Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to find puzzle hash: {puz_hash})"),
                ))
            }
            Some(v) => {
                let secret_key = SecretKey::from_bytes(v.value().0.as_ref()).map_err(|e| {
                    Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e))
                })?;
                let synthetic_secret_key =
                    calculate_synthetic_secret_key(&secret_key, &DEFAULT_HIDDEN_PUZZLE_HASH)?;
                self.secret_key_store.save_secret_key(&synthetic_secret_key);
                Ok(v.value().1)
            }
        }
    }

    async fn add_puzzle_hash_and_keys(
        &self,
        puzzle_hash: Bytes32,
        keys: (Bytes32, Bytes48),
    ) -> Option<(Bytes32, Bytes48)> {
        self.keys_for_ph.insert(puzzle_hash, keys)
    }

    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error> {
        match self
            .secret_key_store()
            .secret_key_for_public_key(public_key)
        {
            None => Err(Error::new(
                ErrorKind::NotFound,
                format!("Failed to find public_key: {public_key})"),
            )),
            Some(v) => {
                let secret_key = SecretKey::from_bytes(v.value().as_ref()).map_err(|e| {
                    Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e))
                })?;
                Ok(secret_key)
            }
        }
    }
}

pub struct MemoryWallet {
    //A wallet that is lost on restarts
    info: WalletInfo<MemoryWalletStore>,
    pub config: MemoryWalletConfig,
    pub fullnode_client: FullnodeClient,
}
impl MemoryWallet {}
#[async_trait]
impl Wallet<MemoryWalletStore, MemoryWalletConfig> for MemoryWallet {
    fn create(info: WalletInfo<MemoryWalletStore>, config: MemoryWalletConfig) -> Self {
        let fullnode_client = FullnodeClient::new(
            &config.fullnode_host.clone(),
            config.fullnode_port,
            60,
            config.fullnode_ssl_path.clone(),
            &config.additional_headers.clone(),
        );
        Self {
            info,
            config,
            fullnode_client,
        }
    }
    fn create_simulator(info: WalletInfo<MemoryWalletStore>, config: MemoryWalletConfig) -> Self {
        let fullnode_client =
            FullnodeClient::new_simulator(&config.fullnode_host.clone(), config.fullnode_port, 60);
        Self {
            info,
            config,
            fullnode_client,
        }
    }

    fn name(&self) -> &str {
        &self.info.name
    }

    async fn sync(&self) -> Result<bool, Error> {
        let standard_coins_arc = self.wallet_store().lock().await.standard_coins().clone();
        let cat_coins_arc = self.wallet_store().lock().await.cat_coins().clone();
        let puzzle_hashes = self
            .wallet_store()
            .lock()
            .await
            .get_puzzle_hashes(0, 100, false)
            .await?;
        let standard_coins = self
            .fullnode_client
            .get_coin_records_by_puzzle_hashes(&puzzle_hashes, Some(true), None, None)
            .await?;
        {
            let mut arc_mut = standard_coins_arc.lock().await;
            arc_mut.clear();
            arc_mut.extend(standard_coins);
        }
        let hinted_coins = self
            .fullnode_client
            .get_coin_records_by_hints(&puzzle_hashes, Some(true), None, None)
            .await?;
        let mut cat_records = vec![];
        for hinted_coin in hinted_coins {
            if let Some(parent_coin) = self
                .fullnode_client
                .get_coin_record_by_name(&hinted_coin.coin.parent_coin_info)
                .await?
            {
                if let Ok(parent_coin_spend) =
                    self.fullnode_client.get_coin_spend(&parent_coin).await
                {
                    let (cat_program, args) =
                        parent_coin_spend.puzzle_reveal.to_program().uncurry()?;
                    let is_cat_v1 = cat_program == *CAT_1_PROGRAM;
                    let is_cat_v2 = !is_cat_v1 && cat_program == *CAT_2_PROGRAM;
                    if is_cat_v1 || is_cat_v2 {
                        let asset_id: Bytes32 = args.rest()?.first()?.try_into()?;
                        let inner_puzzle: Bytes32 = args.rest()?.rest()?.first()?.try_into()?;
                        let lineage_proof = Program::to(vec![
                            parent_coin_spend.coin.parent_coin_info.to_sexp(),
                            inner_puzzle.to_sexp(),
                            parent_coin_spend.coin.amount.to_sexp(),
                        ]);
                        cat_records.push(CatCoinRecord {
                            delegate: hinted_coin,
                            version: if is_cat_v1 {
                                CatVersion::V1
                            } else {
                                CatVersion::V2
                            },
                            asset_id,
                            cat_program,
                            lineage_proof,
                            parent_coin_spend,
                        })
                    } else {
                        error!("Error Parsing Coin as CAT: {hinted_coin:?}")
                    }
                }
            }
        }
        {
            let mut arc_mut = cat_coins_arc.lock().await;
            arc_mut.clear();
            arc_mut.extend(cat_records);
        }
        Ok(true)
    }

    fn is_synced(&self) -> bool {
        todo!()
    }

    fn wallet_info(&self) -> &WalletInfo<MemoryWalletStore> {
        &self.info
    }

    fn wallet_store(&self) -> Arc<Mutex<MemoryWalletStore>> {
        self.info.wallet_store.clone()
    }
}
