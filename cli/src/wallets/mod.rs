use crate::wallets::common::{sign_coin_spends, DerivationRecord};
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dashmap::mapref::one::Ref;
use dashmap::DashMap;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_record::{CatCoinRecord, CoinRecord};
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::transaction_record::{TransactionRecord, TransactionType};
use dg_xch_core::blockchain::wallet_type::{AmountWithPuzzleHash, WalletType};
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::utils::INFINITE_COST;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_keys::{master_sk_to_wallet_sk, master_sk_to_wallet_sk_unhardened};
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    puzzle_for_pk, puzzle_hash_for_pk, solution_for_conditions,
};
use dg_xch_puzzles::utils::{
    make_assert_absolute_seconds_exceeds_condition, make_assert_coin_announcement,
    make_assert_puzzle_announcement, make_create_coin_announcement, make_create_coin_condition,
    make_create_puzzle_announcement, make_reserve_fee_condition,
};
use dg_xch_serialize::{hash_256, ChiaProtocolVersion, ChiaSerialize};
use log::{debug, info};
use num_traits::ToPrimitive;
use rand::prelude::StdRng;
use rand::{Rng, SeedableRng};
use std::cmp::max;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub mod common;
pub mod memory_wallet;
pub mod plotnft_utils;

#[derive(Default)]
pub struct SecretKeyStore {
    keys: DashMap<Bytes48, Bytes32>,
}
impl SecretKeyStore {
    #[must_use]
    pub fn save_secret_key(&self, secret_key: &SecretKey) -> Option<Bytes32> {
        self.keys.insert(
            Bytes48::from(secret_key.sk_to_pk()),
            Bytes32::from(secret_key.to_bytes()),
        )
    }
    #[must_use]
    pub fn secret_key_for_public_key(&self, pub_key: &Bytes48) -> Option<Ref<Bytes48, Bytes32>> {
        self.keys.get(pub_key)
    }
}

#[derive(Eq, PartialEq, Hash, Clone, Copy)]
struct Primary {
    puzzle_hash: Bytes32,
    amount: u64,
}

pub struct WalletInfo<T: WalletStore> {
    pub id: u32,
    pub name: String,
    pub wallet_type: WalletType,
    pub constants: Arc<ConsensusConstants>,
    pub master_sk: SecretKey,
    pub wallet_store: Arc<Mutex<T>>,
    pub data: String, //JSON String to Store Extra Data for Wallets
}

#[async_trait]
pub trait WalletStore {
    fn get_master_sk(&self) -> &SecretKey;
    fn standard_coins(&self) -> Arc<Mutex<Vec<CoinRecord>>>;
    fn cat_coins(&self) -> Arc<Mutex<Vec<CatCoinRecord>>>;
    fn secret_key_store(&self) -> &SecretKeyStore;
    fn current_index(&self) -> u32;
    fn next_index(&self) -> u32;
    async fn get_confirmed_balance(&self) -> u128;
    async fn get_unconfirmed_balance(&self) -> u128;
    async fn get_pending_change_balance(&self) -> u128;
    async fn populate_secret_key_for_puzzle_hash(
        &self,
        puz_hash: &Bytes32,
    ) -> Result<Bytes48, Error>;

    async fn add_puzzle_hash_and_keys(
        &self,
        puzzle_hash: Bytes32,
        keys: (Bytes32, Bytes48),
    ) -> Option<(Bytes32, Bytes48)>;
    async fn get_max_send_amount(&self) -> u128 {
        let unspent: Vec<CoinRecord> = self
            .standard_coins()
            .lock()
            .await
            .iter()
            .filter(|v| !v.spent)
            .copied()
            .collect();
        if unspent.is_empty() {
            0
        } else {
            unspent.iter().map(|v| v.coin.amount as u128).sum()
        }
    }

    async fn get_spendable_balance(&self) -> u128 {
        self.get_max_send_amount().await
    }
    async fn get_puzzle_hashes(
        &self,
        start: u32,
        count: u32,
        hardened: bool,
    ) -> Result<Vec<Bytes32>, Error> {
        let mut puz_hashes = vec![];
        for i in start..start + count {
            puz_hashes.push(
                self.get_derivation_record_at_index(i, hardened)
                    .await?
                    .puzzle_hash,
            );
        }
        Ok(puz_hashes)
    }
    fn wallet_sk(&self, index: u32, hardened: bool) -> Result<SecretKey, Error> {
        if hardened {
            master_sk_to_wallet_sk(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {e:?}")))
        } else {
            master_sk_to_wallet_sk_unhardened(self.get_master_sk(), index)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {e:?}")))
        }
    }
    async fn get_derivation_record_at_index(
        &self,
        index: u32,
        hardened: bool,
    ) -> Result<DerivationRecord, Error> {
        let wallet_sk = self.wallet_sk(index, hardened)?;
        let _ = self.secret_key_store().save_secret_key(&wallet_sk);
        let pubkey = Bytes48::from(wallet_sk.sk_to_pk().to_bytes());
        let puzzle_hash = puzzle_hash_for_pk(&pubkey)?;
        self.add_puzzle_hash_and_keys(puzzle_hash, (Bytes32::from(wallet_sk), pubkey))
            .await;
        Ok(DerivationRecord {
            index,
            puzzle_hash,
            pubkey,
            wallet_type: WalletType::StandardWallet,
            wallet_id: 1,
            hardened,
        })
    }
    async fn get_unused_derivation_record(
        &self,
        hardened: bool,
    ) -> Result<DerivationRecord, Error> {
        self.get_derivation_record_at_index(self.next_index(), hardened)
            .await
    }
    async fn get_derivation_record(&self, hardened: bool) -> Result<DerivationRecord, Error> {
        self.get_derivation_record_at_index(self.current_index(), hardened)
            .await
    }

    #[allow(clippy::too_many_lines)]
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
            for coin_record in self
                .standard_coins()
                .lock()
                .await
                .iter()
                .filter(|v| !v.spent)
            {
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
                valid_spendable_coins.push(coin_record.coin);
            }
            if sum_spendable_coins < amount {
                return Err(Error::new(ErrorKind::InvalidInput, format!("Transaction for {amount} is greater than spendable balance of {sum_spendable_coins}. There may be other transactions pending or our minimum coin amount is too high.")));
            }
            if amount == 0 && sum_spendable_coins == 0 {
                return Err(Error::new(ErrorKind::InvalidInput, "No coins available to spend, you can not create a coin with an amount of 0, without already having coins."));
            }
            valid_spendable_coins.sort_by(|f, s| f.amount.cmp(&s.amount));
            if let Some(c) = check_for_exact_match(&valid_spendable_coins, amount) {
                info!("Selected coin with an exact match: {:?}", c);
                Ok(HashSet::from([c]))
            } else {
                let mut smaller_coin_sum = 0; //coins smaller than target.
                let mut all_sum = 0; //coins smaller than target.
                let mut smaller_coins = vec![];
                for coin in &valid_spendable_coins {
                    if coin.amount < amount {
                        smaller_coin_sum += coin.amount;
                        smaller_coins.push(*coin);
                    }
                    all_sum += coin.amount;
                }
                if smaller_coin_sum == amount && smaller_coins.len() < max_num_coins && amount != 0
                {
                    debug!("Selected all smaller coins because they equate to an exact match of the target: {:?}", smaller_coins);
                    Ok(smaller_coins.iter().copied().collect())
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
                            || coin_set.as_ref().map(HashSet::len).unwrap_or_default()
                                > max_num_coins
                        {
                            let greater_coin =
                                select_smallest_coin_over_target(amount, &valid_spendable_coins);
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
    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error>;
    fn mapping_function<'a, F>(
        &'a self,
        public_key: &'a Bytes48,
    ) -> Box<dyn Future<Output = Result<SecretKey, Error>> + Send + '_> {
        Box::new(self.secret_key_for_public_key(public_key))
    }
}

fn check_for_exact_match(coin_list: &[Coin], target: u64) -> Option<Coin> {
    for coin in coin_list {
        if coin.amount == target {
            return Some(*coin);
        }
    }
    None
}

fn select_smallest_coin_over_target(target: u64, sorted_coin_list: &[Coin]) -> Option<Coin> {
    for coin in sorted_coin_list {
        if coin.amount >= target {
            return Some(*coin);
        }
    }
    None
}

fn sum_largest_coins(target: u128, sorted_coins: &[Coin]) -> Option<HashSet<Coin>> {
    let mut total_value = 0u128;
    let mut selected_coins = HashSet::default();
    for coin in sorted_coins {
        total_value += coin.amount as u128;
        selected_coins.insert(*coin);
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
                    selected_coins.insert(*coin);
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

#[async_trait]
pub trait Wallet<T: WalletStore + Send + Sync, C> {
    fn create(info: WalletInfo<T>, config: C) -> Result<Self, Error> where Self: Sized;
    fn create_simulator(info: WalletInfo<T>, config: C) -> Result<Self, Error> where Self: Sized;
    fn name(&self) -> &str;
    async fn sync(&self) -> Result<bool, Error>;
    fn is_synced(&self) -> bool;
    fn wallet_info(&self) -> &WalletInfo<T>;
    fn wallet_store(&self) -> Arc<Mutex<T>>;
    fn require_derivation_paths(&self) -> bool {
        true
    }
    #[allow(clippy::cast_possible_truncation)]
    async fn puzzle_hashes(
        &self,
        start_index: usize,
        count: usize,
        hardened: bool,
    ) -> Result<Vec<Bytes32>, Error> {
        let mut hashes = vec![];
        for i in start_index..start_index + count {
            hashes.push(
                self.wallet_store()
                    .lock()
                    .await
                    .get_derivation_record_at_index(i as u32, hardened)
                    .await?
                    .puzzle_hash,
            );
        }
        Ok(hashes)
    }
    fn puzzle_for_pk(&self, public_key: &Bytes48) -> Result<Program, Error> {
        puzzle_for_pk(public_key)
    }
    fn puzzle_hash_for_pk(&self, public_key: &Bytes48) -> Result<Bytes32, Error> {
        p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk(public_key)
    }
    #[allow(clippy::too_many_arguments)]
    async fn create_spend_bundle(
        &self,
        payments: Vec<AmountWithPuzzleHash>,
        input_coins: &[CoinRecord],
        change_puzzle_hash: Option<Bytes32>,
        allow_excess: bool,
        fee: i64,
        origin_id: Option<Bytes32>,
        solution_transformer: Option<Box<dyn Fn(Program) -> Program + 'static + Send + Sync>>,
    ) -> Result<SpendBundle, Error>;
    #[allow(clippy::too_many_arguments)]
    fn make_solution(
        &self,
        primaries: &[AmountWithPuzzleHash],
        min_time: u64,
        coin_announcements: Option<HashSet<Vec<u8>>>,
        coin_announcements_to_assert: Option<HashSet<Bytes32>>,
        puzzle_announcements: Option<HashSet<Vec<u8>>>,
        puzzle_announcements_to_assert: Option<HashSet<Bytes32>>,
        fee: u64,
    ) -> Result<Program, Error> {
        let mut condition_list = vec![];
        for primary in primaries {
            condition_list.push(make_create_coin_condition(
                primary.puzzle_hash,
                primary.amount,
                &primary.memos,
            ));
        }
        if min_time > 0 {
            condition_list.push(make_assert_absolute_seconds_exceeds_condition(min_time));
        }
        // if me { //This exists in chia's code but I cant find a usage
        //     condition_list.push(make_assert_my_coin_id_condition(me["id"]));
        // }
        if fee > 0 {
            condition_list.push(make_reserve_fee_condition(fee));
        }
        if let Some(coin_announcements) = coin_announcements {
            for announcement in coin_announcements {
                condition_list.push(make_create_coin_announcement(&announcement));
            }
        }
        if let Some(coin_announcements_to_assert) = coin_announcements_to_assert {
            for announcement_hash in coin_announcements_to_assert {
                condition_list.push(make_assert_coin_announcement(&announcement_hash));
            }
        }
        if let Some(puzzle_announcements) = puzzle_announcements {
            for announcement in puzzle_announcements {
                condition_list.push(make_create_puzzle_announcement(&announcement));
            }
        }
        if let Some(puzzle_announcements_to_assert) = puzzle_announcements_to_assert {
            for announcement_hash in puzzle_announcements_to_assert {
                condition_list.push(make_assert_puzzle_announcement(&announcement_hash));
            }
        }
        solution_for_conditions(condition_list)
    }

    fn compute_memos(
        &self,
        spend_bundle: &SpendBundle,
    ) -> Result<HashMap<Bytes32, Vec<Vec<u8>>>, Error> {
        let mut memos: HashMap<Bytes32, Vec<Vec<u8>>> = HashMap::default();
        for coin_spend in &spend_bundle.coin_spends {
            for (coin_name, coin_memos) in compute_memos_for_spend(coin_spend)? {
                match memos.remove(&coin_name) {
                    Some(mut existing_memos) => {
                        existing_memos.extend(coin_memos);
                        memos.insert(coin_name, existing_memos);
                    }
                    None => {
                        memos.insert(coin_name, coin_memos);
                    }
                }
            }
        }
        Ok(memos)
    }
    async fn puzzle_for_puzzle_hash(&self, puz_hash: &Bytes32) -> Result<Program, Error> {
        let public_key = self
            .wallet_store()
            .lock()
            .await
            .populate_secret_key_for_puzzle_hash(puz_hash)
            .await?;
        puzzle_for_pk(&public_key)
    }
    async fn get_new_puzzle(&self) -> Result<Program, Error> {
        let dr = self
            .wallet_store()
            .lock()
            .await
            .get_unused_derivation_record(false)
            .await?;
        let puzzle = puzzle_for_pk(&dr.pubkey)?;
        self.wallet_store()
            .lock()
            .await
            .populate_secret_key_for_puzzle_hash(&puzzle.tree_hash())
            .await?;
        Ok(puzzle)
    }
    async fn get_puzzle_hash(&self, new: bool) -> Result<Bytes32, Error> {
        Ok(if new {
            self.get_new_puzzlehash().await?
        } else {
            let dr = self
                .wallet_store()
                .lock()
                .await
                .get_derivation_record(false)
                .await?;
            dr.puzzle_hash
        })
    }
    async fn get_new_puzzlehash(&self) -> Result<Bytes32, Error> {
        let dr = self
            .wallet_store()
            .lock()
            .await
            .get_unused_derivation_record(false)
            .await?;
        self.wallet_store()
            .lock()
            .await
            .populate_secret_key_for_puzzle_hash(&dr.puzzle_hash)
            .await?;
        Ok(dr.puzzle_hash)
    }
    async fn convert_puzzle_hash(&self, puzzle_hash: Bytes32) -> Bytes32 {
        puzzle_hash
    }
    async fn generate_simple_signed_transaction(
        &self,
        mojos: u64,
        fee_mojos: u64,
        to_address: Bytes32,
    ) -> Result<TransactionRecord, Error> {
        self.generate_signed_transaction(
            mojos,
            &to_address,
            fee_mojos,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }
    async fn generate_simple_unsigned_transaction(
        &self,
        mojos: u64,
        fee_mojos: u64,
        to_address: Bytes32,
    ) -> Result<Vec<CoinSpend>, Error> {
        self.generate_unsigned_transaction(
            mojos,
            &to_address,
            fee_mojos,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn generate_signed_transaction(
        &self,
        amount: u64,
        puzzle_hash: &Bytes32,
        fee: u64,
        origin_id: Option<Bytes32>,
        coins: Option<Vec<Coin>>,
        primaries: Option<&[AmountWithPuzzleHash]>,
        ignore_max_send_amount: bool,
        coin_announcements_to_consume: Option<&[Announcement]>,
        puzzle_announcements_to_consume: Option<&[Announcement]>,
        memos: Option<Vec<Vec<u8>>>,
        negative_change_allowed: bool,
        min_coin_amount: Option<u64>,
        max_coin_amount: Option<u64>,
        exclude_coin_amounts: Option<&[u64]>,
        exclude_coins: Option<&[Coin]>,
        reuse_puzhash: Option<bool>,
    ) -> Result<TransactionRecord, Error> {
        let non_change_amount = if let Some(primaries) = primaries {
            amount + primaries.iter().map(|a| a.amount).sum::<u64>()
        } else {
            amount
        };
        debug!(
            "Generating transaction for: {} {} {:?}",
            puzzle_hash, amount, coins
        );
        let transaction = self
            .generate_unsigned_transaction(
                amount,
                puzzle_hash,
                fee,
                origin_id,
                coins,
                primaries,
                ignore_max_send_amount,
                coin_announcements_to_consume,
                puzzle_announcements_to_consume,
                memos,
                negative_change_allowed,
                min_coin_amount,
                max_coin_amount,
                exclude_coin_amounts,
                exclude_coins,
                reuse_puzhash,
            )
            .await?;
        assert!(!transaction.is_empty());
        info!("About to sign a transaction: {:?}", transaction);
        let wallet_store = self.wallet_store().clone();
        let spend_bundle = sign_coin_spends(
            transaction,
            |pub_key| {
                let pub_key = *pub_key;
                let wallet_store = wallet_store.clone();
                async move {
                    wallet_store
                        .lock()
                        .await
                        .secret_key_for_public_key(&pub_key)
                        .await
                }
            },
            &self.wallet_info().constants.agg_sig_me_additional_data,
            self.wallet_info()
                .constants
                .max_block_cost_clvm
                .to_u64()
                .unwrap(),
        )
        .await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let add_list = spend_bundle.additions()?;
        let rem_list = spend_bundle.removals();
        let output_amount: u64 = add_list.iter().map(|a| a.amount).sum::<u64>() + fee;
        let input_amount: u64 = rem_list.iter().map(|a| a.amount).sum::<u64>();
        if negative_change_allowed {
            assert!(output_amount >= input_amount);
        } else {
            assert_eq!(output_amount, input_amount);
        }
        let memos = self.compute_memos(&spend_bundle)?;
        let memos = memos
            .into_iter()
            .map(|v| (v.0, v.1))
            .collect::<Vec<(Bytes32, Vec<Vec<u8>>)>>();
        let name = spend_bundle.name();
        Ok(TransactionRecord {
            confirmed_at_height: 0,
            created_at_time: now,
            to_puzzle_hash: *puzzle_hash,
            amount: non_change_amount,
            fee_amount: fee,
            confirmed: false,
            sent: 0,
            spend_bundle: Some(spend_bundle),
            additions: add_list,
            removals: rem_list,
            wallet_id: 1,
            sent_to: vec![],
            trade_id: None,
            transaction_type: TransactionType::OutgoingTx as u32,
            name,
            memos,
        })
    }
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    async fn generate_unsigned_transaction(
        &self,
        amount: u64,
        puzzle_hash: &Bytes32,
        fee: u64,
        origin_id: Option<Bytes32>,
        coins: Option<Vec<Coin>>,
        primaries: Option<&[AmountWithPuzzleHash]>,
        ignore_max_send_amount: bool,
        coin_announcements_to_consume: Option<&[Announcement]>,
        puzzle_announcements_to_consume: Option<&[Announcement]>,
        memos: Option<Vec<Vec<u8>>>,
        negative_change_allowed: bool,
        min_coin_amount: Option<u64>,
        max_coin_amount: Option<u64>,
        exclude_coin_amounts: Option<&[u64]>,
        exclude_coins: Option<&[Coin]>,
        reuse_puzhash: Option<bool>,
    ) -> Result<Vec<CoinSpend>, Error> {
        let mut primaries_amount = 0u64;
        let total_amount: u128;
        if let Some(primaries) = primaries {
            for primary in primaries {
                primaries_amount += primary.amount;
            }
            total_amount = amount as u128 + fee as u128 + primaries_amount as u128;
        } else {
            total_amount = amount as u128 + fee as u128;
        }
        let reuse_puzhash = reuse_puzhash.unwrap_or(true);
        let total_balance = self
            .wallet_store()
            .lock()
            .await
            .get_spendable_balance()
            .await;
        if !ignore_max_send_amount {
            let max_send = self.wallet_store().lock().await.get_max_send_amount().await;
            if total_amount > max_send {
                return Err(Error::new(ErrorKind::InvalidInput, format!("Can't send more than {max_send} mojos in a single transaction, got {total_amount}")));
            }
            debug!("Max send amount: {}", max_send);
        }
        let coins_set: HashSet<Coin>;
        if coins.is_none() {
            if total_amount > total_balance {
                return Err(Error::new(ErrorKind::InvalidInput, format!("Can't spend more than wallet balance: {total_balance} mojos, tried to spend: {total_amount} mojos")));
            }
            coins_set = self
                .wallet_store()
                .lock()
                .await
                .select_coins(
                    total_amount as u64,
                    exclude_coins,
                    min_coin_amount,
                    max_coin_amount.unwrap_or(
                        self.wallet_info()
                            .constants
                            .max_coin_amount
                            .to_u64()
                            .unwrap_or_default(),
                    ),
                    exclude_coin_amounts,
                )
                .await?;
        } else if exclude_coins.is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Can't exclude coins when also specifically including coins",
            ));
        } else {
            coins_set = HashSet::from_iter(coins.unwrap_or_default());
        }
        assert!(!coins_set.is_empty());
        info!("Found Coins to use: {:?}", coins_set);
        let spend_value: i128 = coins_set.iter().map(|v| i128::from(v.amount)).sum::<i128>();
        info!("spend_value is {spend_value} and total_amount is {total_amount}");
        let mut change = spend_value - total_amount as i128;
        if negative_change_allowed {
            change = max(0, change);
        }
        assert!(change >= 0);
        let coin_announcements_bytes = coin_announcements_to_consume
            .unwrap_or_default()
            .iter()
            .map(Announcement::name)
            .collect::<Vec<Bytes32>>();
        let puzzle_announcements_bytes = puzzle_announcements_to_consume
            .unwrap_or_default()
            .iter()
            .map(Announcement::name)
            .collect::<Vec<Bytes32>>();
        let mut spends: Vec<CoinSpend> = vec![];
        let mut primary_announcement_hash = None;
        if primaries.is_some() {
            let mut all_primaries_list = primaries
                .unwrap_or_default()
                .iter()
                .map(|a| Primary {
                    puzzle_hash: a.puzzle_hash,
                    amount: a.amount,
                })
                .collect::<Vec<Primary>>();
            all_primaries_list.push(Primary {
                puzzle_hash: *puzzle_hash,
                amount,
            });
            let as_set: HashSet<Primary> = all_primaries_list.iter().copied().collect();
            if all_primaries_list.len() != as_set.len() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Cannot create two identical coins",
                ));
            }
        }
        let memos = memos.unwrap_or_default();
        let mut origin_id = origin_id;
        for coin in &coins_set {
            if [None, Some(coin.name())].contains(&origin_id) {
                origin_id = Some(coin.name());
                let mut primaries = if let Some(primaries) = primaries {
                    let mut primaries = primaries.to_vec();
                    primaries.push(AmountWithPuzzleHash {
                        amount,
                        puzzle_hash: *puzzle_hash,
                        memos: memos.clone(),
                    });
                    primaries
                } else if amount > 0 {
                    vec![AmountWithPuzzleHash {
                        amount,
                        puzzle_hash: *puzzle_hash,
                        memos: memos.clone(),
                    }]
                } else {
                    vec![]
                };
                if change > 0 {
                    let change_puzzle_hash = if reuse_puzhash {
                        let mut change_puzzle_hash = coin.puzzle_hash;
                        for primary in &primaries {
                            if change_puzzle_hash == primary.puzzle_hash
                                && change == i128::from(primary.amount)
                            {
                                //We cannot create two coins has same id, create a new puzhash for the change:
                                change_puzzle_hash = self.get_new_puzzlehash().await?;
                                break;
                            }
                        }
                        change_puzzle_hash
                    } else {
                        self.get_new_puzzlehash().await?
                    };
                    primaries.push(AmountWithPuzzleHash {
                        amount: change as u64,
                        puzzle_hash: change_puzzle_hash,
                        memos: vec![],
                    });
                }
                let mut message_list: Vec<Bytes32> = coins_set.iter().map(Coin::name).collect();
                for primary in &primaries {
                    message_list.push(
                        Coin {
                            parent_coin_info: coin.name(),
                            puzzle_hash: primary.puzzle_hash,
                            amount: primary.amount,
                        }
                        .name(),
                    );
                }
                let message = hash_256(message_list.iter().fold(vec![], |mut v, e| {
                    v.extend(e.to_bytes(ChiaProtocolVersion::default()));
                    v
                }));
                let coin_announcements = HashSet::from([message.clone()]);
                let coin_announcements_to_assert = HashSet::from_iter(coin_announcements_bytes);
                let puzzle_announcements_to_assert = HashSet::from_iter(puzzle_announcements_bytes);
                info!("Primaries: {:?}", primaries);
                info!(
                    "coin_announcements: {:?}",
                    coin_announcements
                        .iter()
                        .map(|v| { hex::encode(v) })
                        .collect::<Vec<String>>()
                );
                info!(
                    "coin_announcements_to_assert: {:?}",
                    coin_announcements_to_assert
                );
                info!(
                    "puzzle_announcements_to_assert: {:?}",
                    puzzle_announcements_to_assert
                );
                let puzzle = self.puzzle_for_puzzle_hash(&coin.puzzle_hash).await?;
                let solution = self.make_solution(
                    &primaries,
                    0,
                    if coin_announcements.is_empty() {
                        None
                    } else {
                        Some(coin_announcements)
                    },
                    if coin_announcements_to_assert.is_empty() {
                        None
                    } else {
                        Some(coin_announcements_to_assert)
                    },
                    None,
                    if puzzle_announcements_to_assert.is_empty() {
                        None
                    } else {
                        Some(puzzle_announcements_to_assert)
                    },
                    fee,
                )?;
                primary_announcement_hash = Some(
                    Announcement {
                        origin_info: coin.name(),
                        message,
                        morph_bytes: None,
                    }
                    .name(),
                );
                info!("Reveal: {} ", hex::encode(&puzzle.serialized));
                info!("Solution: {} ", hex::encode(&solution.serialized));
                spends.push(CoinSpend {
                    coin: *coin,
                    puzzle_reveal: SerializedProgram::from_bytes(&puzzle.serialized),
                    solution: SerializedProgram::from_bytes(&solution.serialized),
                });
                break;
            }
        }
        //Process the non-origin coins now that we have the primary announcement hash
        for coin in coins_set {
            if Some(coin.name()) == origin_id {
                continue;
            }
            let puzzle = self.puzzle_for_puzzle_hash(&coin.puzzle_hash).await?;
            let solution = self.make_solution(
                &[],
                0,
                None,
                Some(HashSet::from_iter(primary_announcement_hash)),
                None,
                None,
                0,
            )?;
            info!("Reveal: {} ", hex::encode(&puzzle.serialized));
            info!("Solution: {} ", hex::encode(&solution.serialized));
            spends.push(CoinSpend {
                coin,
                puzzle_reveal: SerializedProgram::from_bytes(&puzzle.serialized),
                solution: SerializedProgram::from_bytes(&solution.serialized),
            });
        }
        info!("Spends is {:?}", spends);
        Ok(spends)
    }
}

pub fn compute_memos_for_spend(
    coin_spend: &CoinSpend,
) -> Result<HashMap<Bytes32, Vec<Vec<u8>>>, Error> {
    let (_, result) = coin_spend
        .puzzle_reveal
        .run_with_cost(INFINITE_COST, &coin_spend.solution.to_program())?;
    let mut memos = HashMap::default();
    let result_list = result.as_list();
    for condition in result_list {
        let mut conditions: Vec<Program> = condition.as_list();
        if ConditionOpcode::from(&conditions[0]) == ConditionOpcode::CreateCoin
            && conditions.len() >= 4
        {
            let memo_list = conditions.remove(3);
            let amount = conditions.remove(2);
            let puzzle_hash = conditions.remove(1);
            //If only 3 elements (opcode + 2 args), there is no memo, this is ph, amount
            let coin_added = Coin {
                parent_coin_info: coin_spend.coin.name(),
                puzzle_hash: Bytes32::try_from(puzzle_hash)?,
                amount: amount
                    .as_int()?
                    .to_u64()
                    .ok_or(Error::new(ErrorKind::InvalidInput, "invalid amount"))?,
            };
            let memo_list = memo_list
                .as_list()
                .into_iter()
                .map(|v| v.as_vec().unwrap_or_default())
                .collect::<Vec<Vec<u8>>>();
            memos.insert(coin_added.name(), memo_list);
        }
    }
    Ok(memos)
}
