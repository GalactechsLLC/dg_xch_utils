use crate::wallets::common::{sign_coin_spends, DerivationRecord};
use async_trait::async_trait;
use blst::min_pk::SecretKey;
use dashmap::mapref::one::Ref;
use dashmap::DashMap;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::condition_opcode::ConditionOpcode;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::transaction_record::{TransactionRecord, TransactionType};
use dg_xch_core::blockchain::wallet_type::{AmountWithPuzzlehash, WalletType};
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::utils::INFINITE_COST;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    puzzle_for_pk, solution_for_conditions,
};
use dg_xch_puzzles::utils::{
    make_assert_absolute_seconds_exceeds_condition, make_assert_coin_announcement,
    make_assert_puzzle_announcement, make_create_coin_announcement, make_create_coin_condition,
    make_create_puzzle_announcement, make_reserve_fee_condition,
};
use dg_xch_serialize::{hash_256, ChiaSerialize};
use log::{debug, info};
use num_traits::ToPrimitive;
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
    pub fn save_secret_key(&self, secret_key: &SecretKey) -> Option<Bytes32> {
        self.keys.insert(
            Bytes48::from(secret_key.sk_to_pk()),
            Bytes32::from(secret_key.to_bytes()),
        )
    }
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
    pub constants: ConsensusConstants,
    pub master_sk: SecretKey,
    pub wallet_store: Arc<Mutex<T>>,
    pub data: String, //JSON String to Store Extra Data for Wallets
}

#[async_trait]
pub trait WalletStore {
    fn get_master_sk(&self) -> &SecretKey;
    async fn get_max_send_amount(&self) -> u128;
    async fn get_confirmed_balance(&self) -> u128;
    async fn get_unconfirmed_balance(&self) -> u128;
    async fn get_spendable_balance(&self) -> u128;
    async fn get_pending_change_balance(&self) -> u128;
    async fn get_unused_derivation_record(&self, hardened: bool)
        -> Result<DerivationRecord, Error>;
    async fn get_derivation_record(&self, hardened: bool) -> Result<DerivationRecord, Error>;
    async fn get_derivation_record_at_index(
        &self,
        index: u32,
        hardened: bool,
    ) -> Result<DerivationRecord, Error>;
    async fn select_coins(
        &self,
        amount: u64,
        exclude: Option<&[Coin]>,
        min_coin_amount: Option<u64>,
        max_coin_amount: u64,
        exclude_coin_amounts: Option<&[u64]>,
    ) -> Result<HashSet<Coin>, Error>;
    async fn populate_secret_key_for_puzzle_hash(
        &self,
        puz_hash: &Bytes32,
    ) -> Result<Bytes48, Error>;
    async fn populate_secret_keys_for_coin_spends(
        &self,
        coin_spends: &[CoinSpend],
    ) -> Result<(), Error>;
    async fn secret_key_for_public_key(&self, public_key: &Bytes48) -> Result<SecretKey, Error>;
    fn mapping_function<'a, F>(
        &'a self,
        public_key: &'a Bytes48,
    ) -> Box<dyn Future<Output = Result<SecretKey, Error>> + Send + '_> {
        Box::new(self.secret_key_for_public_key(public_key))
    }
}

#[async_trait]
pub trait Wallet<T: WalletStore + Send + Sync, C> {
    fn create(info: WalletInfo<T>, config: C) -> Self;
    fn name(&self) -> &str;
    async fn sync(&self) -> Result<bool, Error>;
    fn is_synced(&self) -> bool;
    fn wallet_info(&self) -> &WalletInfo<T>;
    fn wallet_store(&self) -> Arc<Mutex<T>>;
    fn require_derivation_paths(&self) -> bool {
        true
    }
    fn puzzle_for_pk(&self, public_key: &Bytes48) -> Result<Program, Error> {
        puzzle_for_pk(public_key)
    }
    fn puzzle_hash_for_pk(&self, public_key: &Bytes48) -> Result<Bytes32, Error> {
        dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk(public_key)
    }
    #[allow(clippy::too_many_arguments)]
    fn make_solution(
        &self,
        primaries: &[AmountWithPuzzlehash],
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
                primary.puzzlehash,
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
                condition_list.push(make_create_coin_announcement(&announcement))
            }
        }
        if let Some(coin_announcements_to_assert) = coin_announcements_to_assert {
            for announcement_hash in coin_announcements_to_assert {
                condition_list.push(make_assert_coin_announcement(&announcement_hash))
            }
        }
        if let Some(puzzle_announcements) = puzzle_announcements {
            for announcement in puzzle_announcements {
                condition_list.push(make_create_puzzle_announcement(&announcement))
            }
        }
        if let Some(puzzle_announcements_to_assert) = puzzle_announcements_to_assert {
            for announcement_hash in puzzle_announcements_to_assert {
                condition_list.push(make_assert_puzzle_announcement(&announcement_hash))
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
            for (coin_name, coin_memos) in compute_memos_for_spend(coin_spend)?.into_iter() {
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
                .get_unused_derivation_record(false)
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
    #[allow(clippy::too_many_arguments)]
    async fn generate_signed_transaction(
        &self,
        amount: u64,
        puzzle_hash: &Bytes32,
        fee: u64,
        origin_id: Option<Bytes32>,
        coins: Option<Vec<Coin>>,
        primaries: Option<&[AmountWithPuzzlehash]>,
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
    async fn generate_unsigned_transaction(
        &self,
        amount: u64,
        puzzle_hash: &Bytes32,
        fee: u64,
        origin_id: Option<Bytes32>,
        coins: Option<Vec<Coin>>,
        primaries: Option<&[AmountWithPuzzlehash]>,
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
            total_amount = amount as u128 + fee as u128 + primaries_amount as u128
        } else {
            total_amount = amount as u128 + fee as u128
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
        let spend_value: i128 = coins_set.iter().map(|v| v.amount as i128).sum::<i128>();
        info!("spend_value is {spend_value} and total_amount is {total_amount}");
        let mut change = spend_value - total_amount as i128;
        if negative_change_allowed {
            change = max(0, change);
        }
        assert!(change >= 0);
        let coin_announcements_bytes = coin_announcements_to_consume
            .unwrap_or_default()
            .iter()
            .map(|a| a.name())
            .collect::<Vec<Bytes32>>();
        let puzzle_announcements_bytes = puzzle_announcements_to_consume
            .unwrap_or_default()
            .iter()
            .map(|a| a.name())
            .collect::<Vec<Bytes32>>();
        let mut spends: Vec<CoinSpend> = vec![];
        let mut primary_announcement_hash = None;
        if primaries.is_some() {
            let mut all_primaries_list = primaries
                .unwrap_or_default()
                .iter()
                .map(|a| Primary {
                    puzzle_hash: a.puzzlehash,
                    amount: a.amount,
                })
                .collect::<Vec<Primary>>();
            all_primaries_list.push(Primary {
                puzzle_hash: *puzzle_hash,
                amount,
            });
            let as_set: HashSet<Primary> = HashSet::from_iter(all_primaries_list.iter().copied());
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
                    primaries.push(AmountWithPuzzlehash {
                        amount,
                        puzzlehash: *puzzle_hash,
                        memos: memos.clone(),
                    });
                    primaries
                } else if amount > 0 {
                    vec![AmountWithPuzzlehash {
                        amount,
                        puzzlehash: *puzzle_hash,
                        memos: memos.clone(),
                    }]
                } else {
                    vec![]
                };
                if change > 0 {
                    let change_puzzle_hash = if reuse_puzhash {
                        let mut change_puzzle_hash = coin.puzzle_hash;
                        for primary in &primaries {
                            if change_puzzle_hash == primary.puzzlehash
                                && change == primary.amount as i128
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
                    primaries.push(AmountWithPuzzlehash {
                        amount: change as u64,
                        puzzlehash: change_puzzle_hash,
                        memos: vec![],
                    });
                }
                let mut message_list: Vec<Bytes32> = coins_set.iter().map(|c| c.name()).collect();
                for primary in &primaries {
                    message_list.push(
                        Coin {
                            parent_coin_info: coin.name(),
                            puzzle_hash: primary.puzzlehash,
                            amount: primary.amount,
                        }
                        .name(),
                    );
                }
                let message = hash_256(&message_list.iter().fold(vec![], |mut v, e| {
                    v.extend(e.to_bytes());
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
                    }
                    .name(),
                );
                info!("Reveal: {} ", hex::encode(&puzzle.serialized));
                info!("Solution: {} ", hex::encode(&solution.serialized));
                spends.push(CoinSpend {
                    coin: coin.clone(),
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
            })
        }
        info!("Spends is {:?}", spends);
        Ok(spends)
    }
}

fn compute_memos_for_spend(
    coin_spend: &CoinSpend,
) -> Result<HashMap<Bytes32, Vec<Vec<u8>>>, Error> {
    let (_, result) = coin_spend
        .puzzle_reveal
        .run_with_cost(INFINITE_COST, &coin_spend.solution.to_program()?)?;
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
                .map(|v| v.serialized)
                .collect::<Vec<Vec<u8>>>();
            memos.insert(coin_added.name(), memo_list);
        }
    }
    Ok(memos)
}
