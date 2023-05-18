use crate::keys::*;
use crate::types::blockchain::sized_bytes::Bytes48;
use crate::wallet::puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use bip39::Mnemonic;
use blst::min_pk::SecretKey;
use log::info;
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

// pub async fn create_plotnft_wallet(full_node_host: String, full_node_port: u16, ) {
//
// }
pub fn create_cold_wallet() -> Result<(), Error> {
    let to_find = [
        "xchluna",
        "xch0xluna",
        "xchluna0x",
        "xchphantom",
        "xch0xphantom",
        "xchchef",
        "xch0xchef",
        "xchchef0x",
        "xch0x",
        "xchevergreen",
        "xchevg",
        "xch0xevg",
        "xchevg0x",
    ];
    let found = AtomicBool::new(false);
    loop {
        if found.load(Ordering::Relaxed) {
            break;
        }
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
        info!("Wallet addresses");
        let limit = 100000;
        let start = Instant::now();
        (0..limit).into_par_iter().for_each(|i| {
            get_address(&to_find, &master_secret_key, i, &found).unwrap();
        });
        let dur = Instant::now().duration_since(start);
        info!(
            "Calculating {} addresses took {}.{} seconds",
            limit,
            dur.as_secs(),
            dur.subsec_millis()
        );
    }
    Ok(())
}

fn get_address(
    to_find: &[&str],
    key: &SecretKey,
    index: u32,
    found: &AtomicBool,
) -> Result<(), Error> {
    let wallet_sk = master_sk_to_wallet_sk(key, index)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("MasterKey: {:?}", e)))?;
    let address = encode_puzzle_hash(
        puzzle_hash_for_pk(&wallet_sk.sk_to_pk().to_bytes().into())?,
        "xch",
    )?;
    for s in to_find {
        if address.starts_with(*s) {
            println!("Index: {}, Found: {}", index, address);
            found.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}
