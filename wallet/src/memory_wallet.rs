use crate::common::{DerivationRecord, sign_coin_spends};
use crate::{SecretKeyStore, Wallet, WalletInfo, WalletStore};
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dashmap::DashMap;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::blockchain::coin_record::{CatCoinRecord, CoinRecord};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::wallet_type::{AmountWithPuzzleHash, WalletType};
use dg_xch_core::clvm::program::Program;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::traits::SizedBytes;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    DEFAULT_HIDDEN_PUZZLE_TREE_HASH, calculate_synthetic_secret_key,
};
use log::{error, info};
use num_traits::ToPrimitive;
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    keys_for_ph: DashMap<Bytes32, (zeroize::Zeroizing<[u8; 32]>, Bytes48)>,
    secret_key_store: SecretKeyStore,
}
impl MemoryWalletStore {
    #[must_use]
    pub fn new(secret_key: SecretKey, starting_index: u32) -> Self {
        Self {
            master_sk: secret_key,
            current_index: AtomicU32::new(starting_index),
            standard_coins: Arc::default(),
            cat_coins: Arc::default(),
            derivation_records: DashMap::default(),
            keys_for_ph: DashMap::default(),
            secret_key_store: SecretKeyStore::default(),
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
        self.current_index
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |index| {
                Some(index.saturating_add(1))
            })
            .map_or(u32::MAX, |index| index.saturating_add(1))
    }

    async fn get_confirmed_balance(&self) -> u128 {
        let coins = self.standard_coins.lock().await;
        coins
            .iter()
            .filter(|coin| !coin.spent)
            .map(|coin| coin.coin.amount as u128)
            .sum()
    }

    async fn get_unconfirmed_balance(&self) -> u128 {
        self.get_confirmed_balance().await
    }

    async fn get_pending_change_balance(&self) -> u128 {
        0
    }

    async fn populate_secret_key_for_puzzle_hash(
        &self,
        puz_hash: &Bytes32,
    ) -> Result<Bytes48, Error> {
        if self.keys_for_ph.is_empty() || self.keys_for_ph.get(puz_hash).is_none() {
            info!("Populating Initial PuzzleHashes");
            let start = self.current_index.load(Ordering::Relaxed);
            let end = start
                .checked_add(100)
                .ok_or_else(|| Error::other("wallet derivation index overflow"))?;
            for i in start..=end {
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
                    Error::new(ErrorKind::InvalidInput, format!("MasterKey: {e:?}"))
                })?;
                let synthetic_secret_key =
                    calculate_synthetic_secret_key(&secret_key, DEFAULT_HIDDEN_PUZZLE_TREE_HASH)?;
                let _old_key = self.secret_key_store.save_secret_key(&synthetic_secret_key);
                Ok(v.value().1)
            }
        }
    }

    async fn add_puzzle_hash_and_keys(
        &self,
        puzzle_hash: Bytes32,
        keys: (Bytes32, Bytes48),
    ) -> Option<(Bytes32, Bytes48)> {
        self.keys_for_ph
            .insert(
                puzzle_hash,
                (zeroize::Zeroizing::new(keys.0.bytes()), keys.1),
            )
            .map(|(secret, public)| (Bytes32::from(*secret), public))
    }

    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error> {
        match self
            .secret_key_store()
            .secret_key_for_public_key(public_key)
        {
            None => Err(Error::new(
                ErrorKind::NotFound,
                format!("Failed to find secret_key for pub_key: {public_key})"),
            )),
            Some(v) => {
                let secret_key = SecretKey::from_bytes(v.value().as_ref()).map_err(|e| {
                    Error::new(ErrorKind::InvalidInput, format!("MasterKey: {e:?}"))
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
    synced: AtomicBool,
}
impl MemoryWallet {
    pub fn new(
        master_secret_key: SecretKey,
        client: &FullnodeClient,
        constants: Arc<ConsensusConstants>,
    ) -> Result<Self, Error> {
        let mut wallet = Self::create(
            WalletInfo {
                id: 1,
                name: "memory_wallet".to_string(),
                wallet_type: WalletType::StandardWallet,
                constants,
                master_sk: master_secret_key.clone(),
                wallet_store: Arc::new(Mutex::new(MemoryWalletStore::new(master_secret_key, 0))),
                data: String::new(),
            },
            MemoryWalletConfig {
                fullnode_host: client.host.clone(),
                fullnode_port: client.port,
                fullnode_ssl_path: client.ssl_path.clone(),
                additional_headers: client.additional_headers.clone(),
            },
        )?;
        wallet.fullnode_client = client.clone();
        Ok(wallet)
    }
    pub async fn public_key(&self, index: u32) -> Result<Bytes48, Error> {
        Ok(self
            .wallet_store()
            .lock()
            .await
            .get_derivation_record_at_index(index, false)
            .await?
            .pubkey)
    }
}
#[async_trait]
impl Wallet<MemoryWalletStore, MemoryWalletConfig> for MemoryWallet {
    fn create(
        info: WalletInfo<MemoryWalletStore>,
        config: MemoryWalletConfig,
    ) -> Result<Self, Error> {
        let fullnode_client = if info.constants.simulated {
            FullnodeClient::new_simulator(&config.fullnode_host.clone(), config.fullnode_port, 60)?
        } else {
            FullnodeClient::new(
                &config.fullnode_host.clone(),
                config.fullnode_port,
                60,
                config.fullnode_ssl_path.clone(),
                &config.additional_headers.clone(),
            )?
        };
        Ok(Self {
            info,
            config,
            fullnode_client,
            synced: AtomicBool::new(false),
        })
    }
    fn create_simulator(
        info: WalletInfo<MemoryWalletStore>,
        config: MemoryWalletConfig,
    ) -> Result<Self, Error> {
        let fullnode_client =
            FullnodeClient::new_simulator(&config.fullnode_host.clone(), config.fullnode_port, 60)?;
        Ok(Self {
            info,
            config,
            fullnode_client,
            synced: AtomicBool::new(false),
        })
    }

    fn name(&self) -> &str {
        &self.info.name
    }

    #[allow(clippy::cast_possible_wrap)]
    async fn sync(&self) -> Result<bool, Error> {
        self.synced.store(false, Ordering::Release);
        let standard_coins_arc = self.wallet_store().lock().await.standard_coins().clone();
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
        self.synced.store(true, Ordering::Release);
        Ok(true)
    }

    fn is_synced(&self) -> bool {
        self.synced.load(Ordering::Acquire)
    }

    fn wallet_info(&self) -> &WalletInfo<MemoryWalletStore> {
        &self.info
    }

    fn wallet_store(&self) -> Arc<Mutex<MemoryWalletStore>> {
        self.info.wallet_store.clone()
    }

    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    async fn create_spend_bundle(
        &self,
        mut payments: Vec<AmountWithPuzzleHash>,
        input_coins: &[CoinRecord],
        change_puzzle_hash: Option<Bytes32>,
        allow_excess: bool,
        fee: i64,
        origin_id: Option<Bytes32>,
        solution_transformer: Option<Box<dyn Fn(Program) -> Program + 'static + Send + Sync>>,
    ) -> Result<SpendBundle, Error> {
        let mut coins = input_coins.to_vec();
        if fee < 0 || coins.is_empty() || coins.iter().any(|coin| coin.spent) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid fee or input coins",
            ));
        }
        let input_ids: HashSet<_> = coins.iter().map(|coin| coin.coin.name()).collect();
        if input_ids.len() != coins.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "duplicate input coin"));
        }
        let total_coin_value: u128 = coins.iter().map(|coin| u128::from(coin.coin.amount)).sum();
        let total_payment_value: u128 = payments
            .iter()
            .map(|payment| u128::from(payment.amount))
            .sum();
        let change = total_coin_value
            .checked_sub(total_payment_value + fee as u128)
            .and_then(|change| u64::try_from(change).ok())
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "insufficient inputs or change overflow",
                )
            })?;
        if change_puzzle_hash.is_none() && change > 0 && !allow_excess {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Found change but not Change Puzzle Hash was provided.",
            ));
        }
        if let Some(change_puzzle_hash) = change_puzzle_hash
            && change > 0
        {
            payments.push(AmountWithPuzzleHash {
                puzzle_hash: change_puzzle_hash,
                amount: change,
                memos: vec![],
            })
        }
        let mut spends = vec![];
        let output_ids: HashSet<_> = payments
            .iter()
            .map(|payment| (payment.puzzle_hash, payment.amount))
            .collect();
        if output_ids.len() != payments.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "duplicate output coin"));
        }
        let origin_index = match origin_id {
            Some(origin_id) => {
                match coins
                    .iter()
                    .enumerate()
                    .find(|(_, val)| val.coin.coin_id() == origin_id)
                {
                    Some((index, _)) => index as i64,
                    None => -1i64,
                }
            }
            None => 0i64,
        };
        if origin_index == -1 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Origin ID Not in Coin List",
            ));
        }
        if origin_index != 0 {
            let origin_coin = coins.remove(origin_index as usize);
            coins.insert(0, origin_coin);
        }
        let mut binding = Vec::new();
        for coin in &coins {
            binding.extend_from_slice(coin.coin.name().as_ref());
        }
        for payment in &payments {
            binding.extend_from_slice(payment.puzzle_hash.as_ref());
            binding.extend_from_slice(&payment.amount.to_be_bytes());
        }
        let message = Bytes32::from(dg_xch_core::utils::hash_256(binding));
        let announcement = |coin: &CoinRecord| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(coin.coin.name().as_ref());
            bytes.extend_from_slice(message.as_ref());
            Bytes32::from(dg_xch_core::utils::hash_256(bytes))
        };
        let origin_announcement = announcement(
            coins
                .first()
                .ok_or_else(|| Error::other("transaction has no input coins"))?,
        );
        for (index, coin) in coins.iter().enumerate() {
            let assertions = if index == 0 {
                coins.iter().skip(1).map(announcement).collect()
            } else {
                HashSet::from([origin_announcement])
            };
            let mut solution = self.make_solution(
                if index == 0 { &payments } else { &[] },
                0,
                Some(HashSet::from([message])),
                Some(assertions),
                None,
                None,
                if index == 0 { fee as u64 } else { 0 },
            )?;
            if let Some(solution_transformer) = &solution_transformer {
                solution = solution_transformer(solution)
            }
            let puzzle = self.puzzle_for_puzzle_hash(&coin.coin.puzzle_hash).await?;
            let coin_spend = CoinSpend {
                coin: coin.coin,
                puzzle_reveal: puzzle.serialized()?,
                solution: solution.serialized()?,
            };
            spends.push(coin_spend);
        }
        info!("Signing Coin Spends");
        let spend_bundle = sign_coin_spends(
            spends,
            |pub_key| {
                let pub_key = *pub_key;
                let wallet_store = self.wallet_store().clone();
                async move {
                    wallet_store
                        .lock()
                        .await
                        .secret_key_for_public_key(&pub_key)
                        .await
                }
            },
            HashMap::with_capacity(0),
            self.wallet_info()
                .constants
                .agg_sig_me_additional_data
                .as_ref(),
            self.wallet_info()
                .constants
                .max_block_cost_clvm
                .to_u64()
                .ok_or_else(|| Error::other("CLVM cost limit exceeds u64"))?,
        )
        .await?;
        Ok(spend_bundle)
    }
}
