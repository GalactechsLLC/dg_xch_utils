use bip39::Mnemonic;
use dg_xch_core::blockchain::sized_bytes::Bytes48;
use dg_xch_keys::*;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use log::info;
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
