use crate::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes, UnsizedBytes};
use crate::consensus::constants::ConsensusConstants;
use blst::min_pk::{AggregatePublicKey, PublicKey, SecretKey};
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::hash_256;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Error, ErrorKind};

pub const NUMBER_ZERO_BITS_PLOT_FILTER: i32 = 9;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofOfSpace {
    pub challenge: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub size: u8,
    pub proof: UnsizedBytes,
}
impl ProofOfSpace {
    pub fn get_plot_id(&self) -> Option<Bytes32> {
        if let (Some(_), Some(_)) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be Some
            None
        } else if let (None, None) = (&self.pool_public_key, &self.pool_contract_puzzle_hash) {
            //Invalid, Both cant be None
            None
        } else if let Some(contract) = &self.pool_contract_puzzle_hash {
            Some(calculate_plot_id_puzzle_hash(
                contract,
                &self.plot_public_key,
            ))
        } else {
            self.pool_public_key
                .as_ref()
                .map(|pub_key| calculate_plot_id_public_key(pub_key, &self.plot_public_key))
        }
    }
}

pub fn calculate_plot_id_public_key(
    pool_public_key: &Bytes48,
    plot_public_key: &Bytes48,
) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_public_key.to_sized_bytes());
    to_hash.extend(plot_public_key.to_sized_bytes());
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    Bytes32::new(hasher.finalize().to_vec())
}

pub fn calculate_plot_id_puzzle_hash(
    pool_contract_puzzle_hash: &Bytes32,
    plot_public_key: &Bytes48,
) -> Bytes32 {
    let mut to_hash: Vec<u8> = Vec::new();
    to_hash.extend(pool_contract_puzzle_hash.to_sized_bytes());
    to_hash.extend(plot_public_key.to_sized_bytes());
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(to_hash);
    Bytes32::new(hasher.finalize().to_vec())
}

pub fn passes_plot_filter(
    constants: &ConsensusConstants,
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> bool {
    let mut filter = [false; 256];
    let mut index = 0;
    for b in calculate_plot_filter_input(plot_id, challenge_hash, signage_point).as_slice() {
        for i in (0..=7).rev() {
            filter[index] = (b >> i & 1) == 1;
            index += 1;
        }
    }
    for is_one in filter.iter().take(constants.number_zero_bits_plot_filter) {
        if *is_one {
            return false;
        }
    }
    true
}

pub fn calculate_plot_filter_input(
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(plot_id);
    hasher.update(challenge_hash);
    hasher.update(signage_point);
    Bytes32::new(hasher.finalize().to_vec())
}

pub fn calculate_pos_challenge(
    plot_id: &Bytes32,
    challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> Bytes32 {
    let mut hasher: Sha256 = Sha256::new();
    hasher.update(calculate_plot_filter_input(
        plot_id,
        challenge_hash,
        signage_point,
    ));
    Bytes32::new(hasher.finalize().to_vec())
}

pub fn generate_taproot_sk(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
) -> Result<SecretKey, Error> {
    let mut taproot_message = vec![];
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    agg.add_public_key(farmer_pk, false)
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
    taproot_message.extend(agg.to_public_key().to_bytes());
    taproot_message.extend(local_pk.to_bytes());
    taproot_message.extend(farmer_pk.to_bytes());
    let taproot_hash = hash_256(&taproot_message);
    SecretKey::key_gen_v3(&taproot_hash, &[])
        .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))
}

pub fn generate_plot_public_key(
    local_pk: &PublicKey,
    farmer_pk: &PublicKey,
    include_taproot: bool,
) -> Result<PublicKey, Error> {
    let mut agg = AggregatePublicKey::from_public_key(local_pk);
    if include_taproot {
        let taproot_sk = generate_taproot_sk(local_pk, farmer_pk)?;
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        agg.add_public_key(&taproot_sk.sk_to_pk(), false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        Ok(agg.to_public_key())
    } else {
        agg.add_public_key(farmer_pk, false)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?;
        Ok(agg.to_public_key())
    }
}
