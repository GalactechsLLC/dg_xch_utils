use crate::types::blockchain::sized_bytes::{Bytes32, SizedBytes};
use bech32::{FromBase32, ToBase32, Variant};
use bip39::Mnemonic;
use blst::min_pk::{PublicKey, SecretKey};
use hkdf::Hkdf;
use sha2::Digest;
use sha2::Sha256;
use std::io::{Error, ErrorKind};
use std::mem::size_of;
use std::str::FromStr;
use crate::wallet::puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;

pub const BLS_SPEC_NUMBER: u32 = 12381;
pub const CHIA_BLOCKCHAIN_NUMBER: u32 = 8444;
pub const FARMER_PATH: u32 = 0;
pub const POOL_PATH: u32 = 1;
pub const WALLET_PATH: u32 = 2;
pub const LOCAL_PATH: u32 = 3;
pub const BACKUP_PATH: u32 = 4;
pub const SINGLETON_PATH: u32 = 5;
pub const POOL_AUTH_PATH: u32 = 6;

pub fn hmac_extract_expand(
    length: usize,
    key: &[u8],
    salt: &[u8],
    info: &[u8],
) -> Result<Vec<u8>, Error> {
    let hk = Hkdf::<Sha256>::new(Some(salt), key);
    let mut out: Vec<u8> = (0..length).map(|_| 0).collect();
    match hk.expand(info, &mut out) {
        Ok(_) => Ok(out),
        Err(e) => Err(Error::new(ErrorKind::InvalidInput, e.to_string())),
    }
}

fn hash_256(to_hash: &[u8]) -> Vec<u8> {
    let mut sha = Sha256::new();
    sha.update(to_hash);
    let res = sha.finalize();
    res.to_vec()
}

fn ikm_to_lamport_sk(ikm: &[u8], salt: &[u8]) -> Result<Vec<u8>, Error> {
    hmac_extract_expand(32 * 255, ikm, salt, &[])
}

fn parent_sk_to_lamport_pk(parent_sk: &SecretKey, index: u32) -> Result<Vec<u8>, Error> {
    let salt = index.to_be_bytes();
    let ikm = parent_sk.to_bytes();
    let not_ikm: Vec<u8> = ikm.into_iter().map(|e| e ^ 0xFF).collect();
    let lamport0 = ikm_to_lamport_sk(&ikm, &salt)?;
    let lamport1 = ikm_to_lamport_sk(&not_ikm, &salt)?;
    let mut lamport_pk = vec![];
    for i in 0..255 {
        lamport_pk.append(&mut hash_256(&lamport0[i * 32..(i + 1) * 32]));
    }
    for i in 0..255 {
        lamport_pk.append(&mut hash_256(&lamport1[i * 32..(i + 1) * 32]));
    }
    Ok(hash_256(&lamport_pk))
}

fn derive_child_sk(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let lamport_pk = parent_sk_to_lamport_pk(key, index)?;
    SecretKey::key_gen_v3(&lamport_pk, &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
}

// fn derive_child_sk_unhardened(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
//     let mut buf = vec![];
//     buf.extend(key.sk_to_pk().to_bytes());
//     buf.extend(index.to_be_bytes());
//     let h = hash_256(&buf);
//     //Currently based on the default curve N
//     let N = BigInt::from_str_radix("0x73EDA753299D7D483339D80809A1D80553BDA402FFFE5BFEFFFFFFFF00000001", 16).unwrap();
//     SecretKey::key_gen_v3(&lamport_pk, &[]).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
// }

pub fn derive_path(key: &SecretKey, paths: Vec<u32>) -> Result<SecretKey, Error> {
    let mut key: SecretKey = key.clone();
    for index in paths {
        key = derive_child_sk(&key, index)?;
    }
    Ok(key)
}

// pub fn derive_path_unhardened(key: &SecretKey, paths: Vec<u32>) -> Result<SecretKey, Error> {
//     let mut key: SecretKey = key.clone();
//     for index in paths {
//         key = derive_child_sk(&key, index)?;
//     }
//     Ok(key)
// }

pub fn master_sk_to_farmer_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, FARMER_PATH, 0])
}

pub fn master_sk_to_pool_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, POOL_PATH, 0])
}

fn master_sk_to_wallet_sk_intermediate(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, WALLET_PATH])
}

pub fn master_sk_to_wallet_sk(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let intermediate = master_sk_to_wallet_sk_intermediate(key)?;
    derive_path(&intermediate, vec![index])
}

// pub fn master_sk_to_wallet_sk_unhardened_intermediate(key: &SecretKey) -> Result<SecretKey, Error> {
//     return _derive_path_unhardened(key, [12381, 8444, 2]);
// }
//
// pub fn master_sk_to_wallet_sk_unhardened(key: &SecretKey, index: uint32) -> Result<SecretKey, Error> {
//     intermediate = master_sk_to_wallet_sk_unhardened_intermediate(key)
//     return _derive_path_unhardened(intermediate, [index]);
// }
pub fn master_sk_to_local_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, LOCAL_PATH, 0])
}

pub fn master_sk_to_backup_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, BACKUP_PATH, 0])
}

pub fn master_sk_to_singleton_owner_sk(
    key: &SecretKey,
    pool_wallet_index: u32,
) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, SINGLETON_PATH, pool_wallet_index])
}

pub fn master_sk_to_pooling_authentication_sk(
    key: &SecretKey,
    pool_wallet_index: u32,
    index: u32,
) -> Result<SecretKey, Error> {
    derive_path(key, vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, POOL_AUTH_PATH, pool_wallet_index * 10000 + index])
}

pub fn key_from_mnemonic(mnemonic: &str) -> Result<SecretKey, Error> {
    let mnemonic = Mnemonic::from_str(mnemonic)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
    let seed = mnemonic.to_seed("");
    SecretKey::key_gen_v3(&seed, &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
}

pub fn fingerprint(key: &PublicKey) -> u32 {
    let mut int_buf = [0; size_of::<u32>()];
    int_buf.copy_from_slice(&hash_256(&key.to_bytes())[0..size_of::<u32>()]);
    u32::from_be_bytes(int_buf)
}

pub fn encode_puzzle_hash(puzzle_hash: &Bytes32, prefix: &str) -> Result<String, Error> {
    bech32::encode(prefix, puzzle_hash.to_bytes().to_base32(), Variant::Bech32m)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
}

pub fn decode_puzzle_hash(address: &str) -> Result<Bytes32, Error> {
    let (_, data, _) = bech32::decode(address)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
    Ok(Bytes32::from(Vec::<u8>::from_base32(&data).map_err(
        |e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)),
    )?))
}
pub fn get_address(key: &SecretKey, index: u32, prefix: &str) -> Result<String, Error> {
    let wallet_sk = master_sk_to_wallet_sk(key, index)?;
    let first_address_hex = puzzle_hash_for_pk(&wallet_sk.sk_to_pk().to_bytes().into())?;
    encode_puzzle_hash(&first_address_hex, prefix)
}
