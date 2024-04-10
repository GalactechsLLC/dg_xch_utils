use crate::wallets::common::DerivationRecord;
use crate::wallets::{SecretKeyStore, Wallet, WalletInfo, WalletStore};
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dashmap::DashMap;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_clients::ClientSSLConfig;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::CoinRecord;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use dg_xch_core::blockchain::wallet_type::WalletType;
use dg_xch_keys::{master_sk_to_wallet_sk, master_sk_to_wallet_sk_unhardened};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    calculate_synthetic_secret_key, puzzle_hash_for_pk, DEFAULT_HIDDEN_PUZZLE_HASH,
};
use log::{debug, error, info};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::{HashMap, HashSet};
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
    pub spent_coins: HashMap<Bytes32, CoinRecord>,
    pub unspent_coins: HashMap<Bytes32, CoinRecord>,
    derivation_records: DashMap<Bytes32, DerivationRecord>,
    keys_for_ph: DashMap<Bytes32, (Bytes32, Bytes48)>,
    secret_key_store: SecretKeyStore,
}
impl MemoryWalletStore {
    pub fn new(secret_key: SecretKey, starting_index: u32) -> Self {
        Self {
            master_sk: secret_key,
            current_index: AtomicU32::new(starting_index),
            spent_coins: Default::default(),
            unspent_coins: Default::default(),
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

    async fn get_max_send_amount(&self) -> u128 {
        if self.unspent_coins.is_empty() {
            0
        } else {
            self.unspent_coins
                .values()
                .map(|v| v.coin.amount as u128)
                .sum()
        }
    }

    async fn get_confirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_unconfirmed_balance(&self) -> u128 {
        todo!()
    }

    async fn get_spendable_balance(&self) -> u128 {
        self.get_max_send_amount().await
    }

    async fn get_pending_change_balance(&self) -> u128 {
        todo!()
    }

    async fn get_unused_derivation_record(
        &self,
        hardened: bool,
    ) -> Result<DerivationRecord, Error> {
        let new_index = self.current_index.fetch_add(1, Ordering::Relaxed);
        let wallet_sk = if hardened {
            master_sk_to_wallet_sk(self.get_master_sk(), new_index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        } else {
            master_sk_to_wallet_sk_unhardened(self.get_master_sk(), new_index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        };
        let pubkey = Bytes48::from(wallet_sk.sk_to_pk().to_bytes());
        let puzzle_hash = puzzle_hash_for_pk(&pubkey)?;
        self.keys_for_ph
            .insert(puzzle_hash, (Bytes32::from(wallet_sk), pubkey));
        Ok(DerivationRecord {
            index: new_index,
            puzzle_hash,
            pubkey,
            wallet_type: WalletType::PoolingWallet,
            wallet_id: 1,
            hardened: false,
        })
    }

    async fn get_derivation_record(&self, hardened: bool) -> Result<DerivationRecord, Error> {
        let index = self.current_index.load(Ordering::Relaxed);
        let wallet_sk = if hardened {
            master_sk_to_wallet_sk(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        } else {
            master_sk_to_wallet_sk_unhardened(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        };
        let pubkey = Bytes48::from(wallet_sk.sk_to_pk().to_bytes());
        let puzzle_hash = puzzle_hash_for_pk(&pubkey)?;
        self.keys_for_ph
            .insert(puzzle_hash, (Bytes32::from(wallet_sk), pubkey));
        Ok(DerivationRecord {
            index,
            puzzle_hash,
            pubkey,
            wallet_type: WalletType::PoolingWallet,
            wallet_id: 1,
            hardened: false,
        })
    }

    async fn get_derivation_record_at_index(
        &self,
        index: u32,
        hardened: bool,
    ) -> Result<DerivationRecord, Error> {
        let wallet_sk = if hardened {
            master_sk_to_wallet_sk(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        } else {
            master_sk_to_wallet_sk_unhardened(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?
        };
        let pubkey = Bytes48::from(wallet_sk.sk_to_pk().to_bytes());
        let puzzle_hash = puzzle_hash_for_pk(&pubkey)?;
        self.keys_for_ph
            .insert(puzzle_hash, (Bytes32::from(wallet_sk), pubkey));
        Ok(DerivationRecord {
            index,
            puzzle_hash,
            pubkey,
            wallet_type: WalletType::PoolingWallet,
            wallet_id: 1,
            hardened: false,
        })
    }

    async fn select_coins(
        &self,
        amount: u64,
        exclude: Option<&[Coin]>,
        min_coin_amount: Option<u64>,
        max_coin_amount: u64,
        exclude_coin_amounts: Option<&[u64]>,
    ) -> Result<HashSet<Coin>, Error> {
        let spendable_amount = self.get_spendable_balance().await;
        let exclude = exclude.unwrap_or_default();
        let min_coin_amount = min_coin_amount.unwrap_or(0);
        let exclude_coin_amounts = exclude_coin_amounts.unwrap_or_default();
        if amount as u128 > spendable_amount {
            Err(Error::new(ErrorKind::InvalidInput, format!("Can't select amount higher than our spendable balance.  Amount: {amount}, spendable: {spendable_amount}")))
        } else {
            debug!("About to select coins for amount {amount}");
            let max_num_coins = 500;
            let mut sum_spendable_coins = 0;
            let mut valid_spendable_coins: Vec<Coin> = vec![];
            for coin_record in self.unspent_coins.values() {
                if exclude.contains(&coin_record.coin) {
                    continue;
                }
                if coin_record.coin.amount < min_coin_amount
                    || coin_record.coin.amount > max_coin_amount
                {
                    continue;
                }
                if exclude_coin_amounts.contains(&coin_record.coin.amount) {
                    continue;
                }
                sum_spendable_coins += coin_record.coin.amount;
                valid_spendable_coins.push(coin_record.coin.clone());
            }
            if sum_spendable_coins < amount {
                return Err(Error::new(ErrorKind::InvalidInput, format!("Transaction for {amount} is greater than spendable balance of {sum_spendable_coins}. There may be other transactions pending or our minimum coin amount is too high.")));
            }
            if amount == 0 && sum_spendable_coins == 0 {
                return Err(Error::new(ErrorKind::InvalidInput, "No coins available to spend, you can not create a coin with an amount of 0, without already having coins."));
            }
            valid_spendable_coins.sort_by(|f, s| f.amount.cmp(&s.amount));
            match check_for_exact_match(&valid_spendable_coins, amount) {
                Some(c) => {
                    info!("Selected coin with an exact match: {:?}", c);
                    Ok(HashSet::from([c]))
                }
                None => {
                    let mut smaller_coin_sum = 0; //coins smaller than target.
                    let mut all_sum = 0; //coins smaller than target.
                    let mut smaller_coins = vec![];
                    for coin in &valid_spendable_coins {
                        if coin.amount < amount {
                            smaller_coin_sum += coin.amount;
                            smaller_coins.push(coin.clone());
                        }
                        all_sum += coin.amount;
                    }
                    if smaller_coin_sum == amount
                        && smaller_coins.len() < max_num_coins
                        && amount != 0
                    {
                        debug!("Selected all smaller coins because they equate to an exact match of the target: {:?}", smaller_coins);
                        Ok(HashSet::from_iter(smaller_coins.iter().cloned()))
                    } else if smaller_coin_sum < amount {
                        let smallest_coin =
                            select_smallest_coin_over_target(amount, &valid_spendable_coins);
                        if let Some(smallest_coin) = smallest_coin {
                            debug!("Selected closest greater coin: {}", smallest_coin.name());
                            Ok(HashSet::from([smallest_coin]))
                        } else {
                            return Err(Error::new(ErrorKind::InvalidInput, format!("Transaction of {amount} mojo is greater than available sum {all_sum} mojos.")));
                        }
                    } else if smaller_coin_sum > amount {
                        let mut coin_set = knapsack_coin_algorithm(
                            &smaller_coins,
                            amount,
                            max_coin_amount,
                            max_num_coins,
                            None,
                        );
                        debug!("Selected coins from knapsack algorithm: {:?}", coin_set);
                        if coin_set.is_none() {
                            coin_set = sum_largest_coins(amount as u128, &smaller_coins);
                            if coin_set.is_none()
                                || coin_set.as_ref().map(|v| v.len()).unwrap_or_default()
                                    > max_num_coins
                            {
                                let greater_coin = select_smallest_coin_over_target(
                                    amount,
                                    &valid_spendable_coins,
                                );
                                if let Some(greater_coin) = greater_coin {
                                    coin_set = Some(HashSet::from([greater_coin]));
                                } else {
                                    return Err(Error::new(ErrorKind::InvalidInput, format!("Transaction of {amount} mojo would use more than {max_num_coins} coins. Try sending a smaller amount")));
                                }
                            }
                        }
                        coin_set.ok_or_else(|| {
                            Error::new(
                                ErrorKind::InvalidInput,
                                "Failed to select coins for transaction",
                            )
                        })
                    } else {
                        match select_smallest_coin_over_target(amount, &valid_spendable_coins) {
                            Some(coin) => {
                                debug!("Resorted to selecting smallest coin over target due to dust.: {:?}", coin);
                                Ok(HashSet::from([coin]))
                            }
                            None => Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Too many coins are required to make this transaction",
                            )),
                        }
                    }
                }
            }
        }
    }

    async fn populate_secret_key_for_puzzle_hash(
        &self,
        puz_hash: &Bytes32,
    ) -> Result<Bytes48, Error> {
        if self.keys_for_ph.is_empty() || self.keys_for_ph.get(puz_hash).is_none() {
            info!("Populating Initial PuzzleHashes");
            for i in 0..=100 {
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

    async fn populate_secret_keys_for_coin_spends(
        &self,
        coin_spends: &[CoinSpend],
    ) -> Result<(), Error> {
        for coin_spend in coin_spends {
            self.populate_secret_key_for_puzzle_hash(&coin_spend.coin.puzzle_hash)
                .await?;
        }
        Ok(())
    }

    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error> {
        match self.secret_key_store.secret_key_for_public_key(public_key) {
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

fn check_for_exact_match(coin_list: &[Coin], target: u64) -> Option<Coin> {
    for coin in coin_list {
        if coin.amount == target {
            return Some(coin.clone());
        }
    }
    None
}

fn select_smallest_coin_over_target(target: u64, sorted_coin_list: &[Coin]) -> Option<Coin> {
    for coin in sorted_coin_list.iter() {
        if coin.amount >= target {
            return Some(coin.clone());
        }
    }
    None
}

fn sum_largest_coins(target: u128, sorted_coins: &[Coin]) -> Option<HashSet<Coin>> {
    let mut total_value = 0u128;
    let mut selected_coins = HashSet::default();
    for coin in sorted_coins {
        total_value += coin.amount as u128;
        selected_coins.insert(coin.clone());
        if total_value >= target {
            return Some(selected_coins);
        }
    }
    None
}

fn knapsack_coin_algorithm(
    smaller_coins: &[Coin],
    target: u64,
    max_coin_amount: u64,
    max_num_coins: usize,
    seed: Option<&[u8]>,
) -> Option<HashSet<Coin>> {
    let mut best_set_sum = max_coin_amount;
    let mut best_set_of_coins: Option<HashSet<Coin>> = None;
    let seed = Bytes32::new(seed.unwrap_or(b"knapsack seed"));
    let mut rand = StdRng::from_seed(*seed.to_sized_bytes());
    for _ in 0..1000 {
        let mut selected_coins = HashSet::default();
        let mut selected_coins_sum = 0;
        let mut n_pass = 0;
        let mut target_reached = false;
        while n_pass < 2 && !target_reached {
            for coin in smaller_coins {
                if (n_pass == 0 && rand.gen::<bool>())
                    || (n_pass == 1 && !selected_coins.contains(coin))
                {
                    if selected_coins.len() > max_num_coins {
                        break;
                    }
                    selected_coins_sum += coin.amount;
                    selected_coins.insert(coin.clone());
                    match selected_coins_sum.cmp(&target) {
                        std::cmp::Ordering::Greater => {
                            target_reached = true;
                            if selected_coins_sum < best_set_sum {
                                best_set_of_coins = Some(selected_coins.clone());
                                best_set_sum = selected_coins_sum;
                                selected_coins_sum -= coin.amount;
                                selected_coins.remove(coin);
                            }
                        }
                        std::cmp::Ordering::Less => {}
                        std::cmp::Ordering::Equal => return Some(selected_coins),
                    }
                }
            }
            n_pass += 1;
        }
    }
    best_set_of_coins
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

    fn name(&self) -> &str {
        &self.info.name
    }

    async fn sync(&self) -> Result<bool, Error> {
        todo!()
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
