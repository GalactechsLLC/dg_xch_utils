use bech32::{Bech32m, Hrp};
use bip39::Mnemonic;
use blst::min_pk::{PublicKey, SecretKey};
use blst::{blst_bendian_from_scalar, blst_scalar, blst_scalar_from_be_bytes, blst_sk_add_n_check};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::formatting::prep_hex_str;
use dg_xch_core::traits::SizedBytes;
use dg_xch_core::utils::hash_256;
use dg_xch_puzzles::p2_delegated_puzzle_or_hidden_puzzle::puzzle_hash_for_pk;
use hkdf::Hkdf;
use sha2::Sha256;
use std::io::{Error, ErrorKind};
use std::mem::size_of;
use std::str::FromStr;
use zeroize::Zeroizing;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

pub const BLS_SPEC_NUMBER: u32 = 12381;
pub const CHIA_BLOCKCHAIN_NUMBER: u32 = 8444;
pub const FARMER_PATH: u32 = 0;
pub const POOL_PATH: u32 = 1;
pub const WALLET_PATH: u32 = 2;
pub const LOCAL_PATH: u32 = 3;
pub const BACKUP_PATH: u32 = 4;
pub const SINGLETON_PATH: u32 = 5;
pub const POOL_AUTH_PATH: u32 = 6;

pub fn hmac_extract_expand<const N: usize>(
    key: &[u8],
    salt: &[u8],
    info: &[u8],
) -> Result<[u8; N], Error> {
    let hk = Hkdf::<Sha256>::new(Some(salt), key);
    let mut out: [u8; N] = [0; N];
    match hk.expand(info, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => Err(Error::new(ErrorKind::InvalidInput, e.to_string())),
    }
}

fn ikm_to_lamport_sk(ikm: &[u8], salt: &[u8]) -> Result<[u8; 8160], Error> {
    // 32 * 255
    hmac_extract_expand::<8160>(ikm, salt, &[])
}

fn parent_sk_to_lamport_pk(parent_sk: &SecretKey, index: u32) -> Result<Bytes32, Error> {
    let salt = index.to_be_bytes();
    let ikm = parent_sk.to_bytes();
    let not_ikm: Vec<u8> = ikm.into_iter().map(|e| e ^ 0xFF).collect();
    let lamport0 = ikm_to_lamport_sk(&ikm, &salt)?;
    let lamport1 = ikm_to_lamport_sk(&not_ikm, &salt)?;
    let mut lamport_pk = Vec::with_capacity(lamport0.len() + lamport1.len());
    for i in 0..255 {
        lamport_pk.extend(hash_256(&lamport0[i * 32..(i + 1) * 32]));
    }
    for i in 0..255 {
        lamport_pk.extend(hash_256(&lamport1[i * 32..(i + 1) * 32]));
    }
    Ok(Bytes32::new(hash_256(&lamport_pk)))
}

fn derive_child_sk(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let lamport_pk = parent_sk_to_lamport_pk(key, index)?;
    SecretKey::key_gen_v3(lamport_pk.as_ref(), &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
}

fn derive_child_sk_unhardened(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let mut buf = vec![];
    buf.extend(key.sk_to_pk().to_bytes());
    buf.extend(index.to_be_bytes());
    let hash = hash_256(&buf);
    let kb = key.to_bytes();
    let mut out = [0u8; 32];
    let mut o = blst_scalar::default();
    let mut h = blst_scalar::default();
    let mut s = blst_scalar::default();
    let agg = unsafe {
        blst_scalar_from_be_bytes(&mut h, hash.as_ptr(), hash.len());
        blst_scalar_from_be_bytes(&mut s, kb.as_ptr(), kb.len());
        blst_sk_add_n_check(&mut o, &h, &s);
        blst_bendian_from_scalar(out.as_mut_ptr(), &o);
        out
    };
    SecretKey::from_bytes(&agg).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
}

pub fn derive_path(key: &SecretKey, paths: Vec<u32>) -> Result<SecretKey, Error> {
    let mut key: SecretKey = key.clone();
    for index in paths {
        key = derive_child_sk(&key, index)?;
    }
    Ok(key)
}

pub fn derive_path_unhardened(key: &SecretKey, paths: Vec<u32>) -> Result<SecretKey, Error> {
    let mut key: SecretKey = key.clone();
    for index in paths {
        key = derive_child_sk_unhardened(&key, index)?;
    }
    Ok(key)
}

pub fn master_sk_to_farmer_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, FARMER_PATH, 0],
    )
}

pub fn master_sk_to_pool_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, POOL_PATH, 0],
    )
}

fn master_sk_to_wallet_sk_intermediate(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, WALLET_PATH],
    )
}

pub fn master_sk_to_wallet_sk(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let intermediate = master_sk_to_wallet_sk_intermediate(key)?;
    derive_path(&intermediate, vec![index])
}

pub fn master_sk_to_wallet_sk_unhardened_intermediate(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path_unhardened(key, vec![12381, 8444, 2])
}

pub fn master_sk_to_wallet_sk_unhardened(key: &SecretKey, index: u32) -> Result<SecretKey, Error> {
    let intermediate = master_sk_to_wallet_sk_unhardened_intermediate(key)?;
    derive_path_unhardened(&intermediate, vec![index])
}

pub fn master_sk_to_local_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, LOCAL_PATH, 0],
    )
}

pub fn master_sk_to_backup_sk(key: &SecretKey) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![BLS_SPEC_NUMBER, CHIA_BLOCKCHAIN_NUMBER, BACKUP_PATH, 0],
    )
}

pub fn master_sk_to_singleton_owner_sk(
    key: &SecretKey,
    pool_wallet_index: u32,
) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![
            BLS_SPEC_NUMBER,
            CHIA_BLOCKCHAIN_NUMBER,
            SINGLETON_PATH,
            pool_wallet_index,
        ],
    )
}

pub fn master_sk_to_pooling_authentication_sk(
    key: &SecretKey,
    pool_wallet_index: u32,
    index: u32,
) -> Result<SecretKey, Error> {
    derive_path(
        key,
        vec![
            BLS_SPEC_NUMBER,
            CHIA_BLOCKCHAIN_NUMBER,
            POOL_AUTH_PATH,
            pool_wallet_index * 10000 + index,
        ],
    )
}

pub fn key_from_mnemonic_str(mnemonic: &str) -> Result<SecretKey, Error> {
    let mnemonic = Mnemonic::from_str(mnemonic)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
    key_from_mnemonic(&mnemonic)
}

pub fn key_from_mnemonic(mnemonic: &Mnemonic) -> Result<SecretKey, Error> {
    let seed = Zeroizing::new(mnemonic.to_seed(""));
    SecretKey::key_gen_v3(seed.as_ref(), &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
}

pub fn random_key() -> Result<SecretKey, Error> {
    let mnemonic = Mnemonic::generate(24)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))?;
    key_from_mnemonic(&mnemonic)
}

#[must_use]
pub fn fingerprint(key: &PublicKey) -> u32 {
    let mut int_buf = [0; size_of::<u32>()];
    int_buf.copy_from_slice(&hash_256(key.to_bytes())[0..size_of::<u32>()]);
    u32::from_be_bytes(int_buf)
}

pub fn encode_puzzle_hash(puzzle_hash: &Bytes32, prefix: &str) -> Result<String, Error> {
    bech32::encode::<Bech32m>(Hrp::parse_unchecked(prefix), &puzzle_hash.bytes())
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{e:?}")))
}

pub fn decode_puzzle_hash(address: &str) -> Result<Bytes32, Error> {
    let (_, data) = bech32::decode(address).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Error Decoding address: ({address}): {e:?}"),
        )
    })?;
    Bytes32::parse(&data).map_err(|e| {
        Error::new(
            ErrorKind::InvalidInput,
            format!("Error Decoding address: ({address}): {e:?}"),
        )
    })
}
pub fn get_address(key: &SecretKey, index: u32, prefix: &str) -> Result<String, Error> {
    let wallet_sk = master_sk_to_wallet_sk(key, index)?;
    let address_hex = puzzle_hash_for_pk(wallet_sk.sk_to_pk().to_bytes().into())?;
    encode_puzzle_hash(&address_hex, prefix)
}

pub fn parse_payout_address(s: &str) -> Result<String, Error> {
    if let Ok(puzzle_hash) = decode_puzzle_hash(s) {
        return Ok(hex::encode(puzzle_hash));
    }
    let clean_hex = prep_hex_str(s);
    if clean_hex.len() == 64 {
        //Should be a pointless conversion, validates the string is hex
        let decoded = hex::decode(&clean_hex).map_err(|e| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("Error Parsing Payout Address({s}): {e:?}"),
            )
        })?;
        Ok(hex::encode(decoded))
    } else {
        Err(Error::new(
            ErrorKind::InvalidInput,
            "String does not appear to be a valid payout address or puzzle hash",
        ))
    }
}

#[cfg(test)]
mod payout_tests {
    use super::*;

    #[test]
    fn payout_addresses_accept_chia_and_custom_prefixes() {
        let puzzle_hash = Bytes32::from([37; 32]);
        let expected = hex::encode(puzzle_hash);
        for prefix in ["xch", "txch", "dgx", "custom"] {
            let address = encode_puzzle_hash(&puzzle_hash, prefix).unwrap();
            assert_eq!(parse_payout_address(&address).unwrap(), expected);
        }
        assert_eq!(parse_payout_address(&expected).unwrap(), expected);
        assert_eq!(
            parse_payout_address(&format!("0x{expected}")).unwrap(),
            expected
        );
    }

    #[test]
    fn payout_addresses_reject_invalid_checksums_and_lengths() {
        let address = encode_puzzle_hash(&Bytes32::from([37; 32]), "dgx").unwrap();
        let mut invalid = address.into_bytes();
        let last = invalid.last_mut().unwrap();
        *last = if *last == b'q' { b'p' } else { b'q' };
        assert!(parse_payout_address(std::str::from_utf8(&invalid).unwrap()).is_err());
        for invalid in ["dgx1", "", "1234", &"z".repeat(64)] {
            assert!(parse_payout_address(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod mnemonic_tests {
    use super::{key_from_mnemonic, key_from_mnemonic_str};
    use bip39::Mnemonic;
    use blst::min_pk::SecretKey;

    #[test]
    fn zeroized_seed_handling_preserves_key_derivation() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let mnemonic = phrase.parse::<Mnemonic>().unwrap();
        let expected = SecretKey::key_gen_v3(&mnemonic.to_seed(""), &[]).unwrap();
        assert_eq!(
            key_from_mnemonic(&mnemonic).unwrap().to_bytes(),
            expected.to_bytes()
        );
        assert_eq!(
            key_from_mnemonic_str(phrase).unwrap().to_bytes(),
            expected.to_bytes()
        );
    }

    #[test]
    fn mnemonic_objects_are_zeroized_on_drop() {
        fn requires_zeroize_on_drop<Secret: zeroize::ZeroizeOnDrop>() {}
        requires_zeroize_on_drop::<Mnemonic>();
    }
}
