use std::collections::{HashMap, HashSet};
use bip39::Mnemonic;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_keys::*;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{calculate_synthetic_secret_key, DEFAULT_HIDDEN_PUZZLE_HASH, puzzle_hash_for_pk};
use log::{debug, info};
use std::io::{Error, ErrorKind};
use std::time::{SystemTime, UNIX_EPOCH};
use blst::min_pk::SecretKey;
use num_traits::ToPrimitive;
use dg_xch_clients::api::full_node::FullnodeAPI;
use dg_xch_clients::rpc::full_node::FullnodeClient;
use dg_xch_core::blockchain::announcement::Announcement;
use dg_xch_core::blockchain::coin::Coin;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::transaction_record::{TransactionRecord, TransactionType};
use dg_xch_core::blockchain::wallet_type::AmountWithPuzzlehash;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::pool::PoolState;
use dg_xch_puzzles::clvm_puzzles::{get_most_recent_singleton_coin_from_coin_spend, solution_to_pool_state};
use crate::commands::sign_coin_spends;

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

pub async fn get_current_pool_state(client: &FullnodeClient, launcher_id: &Bytes32) -> Result<(PoolState, CoinSpend), Error> {
    let mut last_spend: CoinSpend;
    let mut saved_state: PoolState;
    match client.get_coin_record_by_name(launcher_id).await? {
        Some(lc) if lc.spent => {
            last_spend = client.get_coin_spend(&lc).await?;
            match solution_to_pool_state(&last_spend)? {
                Some(state) => {
                    saved_state = state;
                }
                None => {
                    return Err(Error::new(ErrorKind::InvalidData, "Failed to Read Pool State"));
                }
            }
        }
        Some(_) => {
            return Err(Error::new(ErrorKind::InvalidData, format!("Genesis coin {} not spent", &launcher_id.to_string())));
        }
        None => {
            return Err(Error::new(ErrorKind::NotFound, format!("Can not find genesis coin {}", &launcher_id)));
        }
    }
    let mut saved_spend: CoinSpend = last_spend.clone();
    let mut last_not_none_state: PoolState = saved_state.clone();
    loop {
        match get_most_recent_singleton_coin_from_coin_spend(&last_spend)?{
            None => {
                return Err(Error::new(ErrorKind::NotFound,"Failed to find recent singleton from coin Record"));
            }
            Some(next_coin) => {
                match client.get_coin_record_by_name(&next_coin.name()).await? {
                    None => {
                        return Err(Error::new(ErrorKind::NotFound,"Failed to find Coin Record"));
                    }
                    Some(next_coin_record) => {
                        if !next_coin_record.spent {
                            break;
                        }
                        last_spend = client.get_coin_spend(&next_coin_record).await?;
                        if let Ok(Some(pool_state)) = solution_to_pool_state(&last_spend) {
                            last_not_none_state = pool_state;
                        }
                        saved_spend = last_spend.clone();
                        saved_state = last_not_none_state.clone();
                    }
                }
            }
        }
    }
    Ok((saved_state, saved_spend))
}

pub fn find_owner_key(master_secret_key: &SecretKey, key_to_find: &Bytes48, limit: u32) -> Result<SecretKey, Error> {
    for i in 0..limit {
        let key = master_sk_to_singleton_owner_sk(master_secret_key, i)?;
        if &key.sk_to_pk().to_bytes() == key_to_find.to_sized_bytes() {
            return Ok(key);
        }
    }
    Err(Error::new(ErrorKind::NotFound, "Failed to find Owner SK"))
}

pub async fn generate_fee_transaction(master_secret_key: &SecretKey, fee: u64, puz_hash: &Bytes32, coin_announcements: Option<&[Announcement]>, constants: &ConsensusConstants) -> Result<TransactionRecord, Error> {
    generate_signed_transaction(
        0,
        puz_hash,
        fee,
        None,
        None,
        None,
        false,
        coin_announcements,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        None,
        constants,
        master_secret_key
    ).await
}

pub async fn generate_signed_transaction(
    amount: u64,
    puzzle_hash: &Bytes32,
    fee: u64,
    origin_id: Option<&Bytes32>,
    coins: Option<&[Coin]>,
    primaries: Option<&[AmountWithPuzzlehash]>,
    ignore_max_send_amount: bool,
    coin_announcements_to_consume: Option<&[Announcement]>,
    puzzle_announcements_to_consume: Option<&[Announcement]>,
    memos: Option<&[&[u8]]>,
    negative_change_allowed: bool,
    min_coin_amount: Option<u64>,
    max_coin_amount: Option<u64>,
    exclude_coin_amounts: Option<&[u64]>,
    exclude_coins: Option<&[Coin]>,
    reuse_puzhash: Option<bool>,
    constants: &ConsensusConstants,
    master_sk: &SecretKey,
) -> Result<TransactionRecord, Error> {
    let non_change_amount = if let Some(primaries) = primaries {
        amount + primaries.iter().map(|a| a.amount).sum::<u64>()
    } else {
        amount
    };
    debug!("Generating transaction for: {} {} {:?}", puzzle_hash, amount, coins);
    let transaction = generate_unsigned_transaction(
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
    ).await?;
    assert!(!transaction.is_empty());
    info!("About to sign a transaction: {:?}", transaction);
    let key_map = keys_for_coinspends(&transaction, master_sk, 500)?;
    let spend_bundle = sign_coin_spends(
        transaction,
        |k| {
            key_map.get(k).cloned().ok_or_else( || {
                Error::new(
                    ErrorKind::NotFound,
                    format!("Failed to find secret key for: {:?}", k),
                )
            })
        },
        &constants.agg_sig_me_additional_data,
        constants.max_block_cost_clvm.to_u64().unwrap(),
    ).await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let add_list = spend_bundle.additions()?;
    let rem_list = spend_bundle.removals();
    let output_amount: u64 = add_list.iter().map(|a| a.amount).sum::<u64>() + fee;
    let input_amount: u64 = rem_list.iter().map(|a| a.amount).sum::<u64>();
    if negative_change_allowed {
        assert!(output_amount >= input_amount);
    } else {
        assert_eq!(output_amount, input_amount);
    }
    let memos = compute_memos(&spend_bundle)?;
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

type Memo = (Bytes32, Vec<Vec<u8>>);

pub fn compute_memos(_spend_bundle: &SpendBundle) -> Result<Vec<Memo>, Error> {
    todo!()
}

pub fn keys_for_coinspends(coin_spends: &[CoinSpend], master_sk: &SecretKey, max_pub_keys: u32) -> Result<HashMap<Bytes48, SecretKey>, Error>{
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
                let synthetic_secret_key = calculate_synthetic_secret_key(&sec_key, &DEFAULT_HIDDEN_PUZZLE_HASH)?;
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

pub async fn generate_unsigned_transaction(
    _amount: u64,
    _newpuzzlehash: &Bytes32,
    _fee: u64,
    _origin_id: Option<&Bytes32>,
    _coins: Option<&[Coin]>,
    _primaries_input: Option<&[AmountWithPuzzlehash]>,
    _ignore_max_send_amount: bool,
    _coin_announcements_to_consume: Option<&[Announcement]>,
    _puzzle_announcements_to_consume: Option<&[Announcement]>,
    _memos: Option<&[&[u8]]>,
    _negative_change_allowed: bool,
    _min_coin_amount: Option<u64>,
    _max_coin_amount: Option<u64>,
    _exclude_coin_amounts: Option<&[u64]>,
    _exclude_coins: Option<&[Coin]>,
    _reuse_puzhash: Option<bool>,
) -> Result<Vec<CoinSpend>, Error> {
    todo!()
}