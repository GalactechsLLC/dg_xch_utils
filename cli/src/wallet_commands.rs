use bip39::Mnemonic;
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::coin_spend::CoinSpend;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_keys::*;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::{
    calculate_synthetic_secret_key, puzzle_hash_for_pk, DEFAULT_HIDDEN_PUZZLE_HASH,
};
use log::{info};
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};

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
