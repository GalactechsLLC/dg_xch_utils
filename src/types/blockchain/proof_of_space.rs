use crate::clvm::utils::hash_256;
use crate::consensus::constants::ConsensusConstants;
use crate::proof_of_space::verifier::validate_proof;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes48, SizedBytes, UnsizedBytes};
use crate::types::ChiaSerialize;
use blst::min_pk::{AggregatePublicKey, PublicKey, SecretKey};
use log::warn;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Error, ErrorKind};

pub const NUMBER_ZERO_BITS_PLOT_FILTER: i32 = 9;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofOfSpace {
    pub challenge: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub size: u8,
    pub proof: UnsizedBytes,
}
impl ChiaSerialize for ProofOfSpace {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.challenge.to_sized_bytes());
        match &self.pool_public_key {
            Some(public_key) => {
                bytes.push(1u8);
                bytes.extend(public_key.to_sized_bytes());
            }
            None => {
                bytes.push(0u8);
            }
        }
        match &self.pool_contract_puzzle_hash {
            Some(contract_hash) => {
                bytes.push(1u8);
                bytes.extend(contract_hash.to_sized_bytes());
            }
            None => {
                bytes.push(0u8);
            }
        }
        bytes.extend(self.plot_public_key.to_sized_bytes());
        bytes.push(self.size);
        bytes.extend((self.proof.bytes.len() as u32).to_be_bytes());
        bytes.extend(&self.proof.bytes);
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, std::io::Error>
    where
        Self: Sized,
    {
        let (challenge, rest) = bytes.split_at(32);

        let (has_public_key, mut rest) = rest.split_at(1);
        let pool_public_key;
        if has_public_key[0] > 0 {
            let (p, r) = rest.split_at(48);
            pool_public_key = Some(p.into());
            rest = r;
        } else {
            pool_public_key = None;
        }

        let (has_contract, mut rest) = rest.split_at(1);
        let pool_contract_puzzle_hash;
        if has_contract[0] > 0 {
            let (p, r) = rest.split_at(32);
            pool_contract_puzzle_hash = Some(p.into());
            rest = r;
        } else {
            pool_contract_puzzle_hash = None;
        }
        let (plot_public_key, rest) = rest.split_at(48);

        let (size, rest) = rest.split_at(1);

        let mut u32_len_ary: [u8; 4] = [0; 4];
        let (proof_len, rest) = rest.split_at(4);
        u32_len_ary.copy_from_slice(&proof_len[0..4]);
        let proof_len = u32::from_be_bytes(u32_len_ary) as usize;
        let (proof_of_space, _) = rest.split_at(proof_len);
        Ok(Self {
            challenge: challenge.into(),
            pool_contract_puzzle_hash,
            plot_public_key: plot_public_key.into(),
            pool_public_key,
            proof: proof_of_space.into(),
            size: size[0],
        })
    }
}
impl ProofOfSpace {
    fn get_plot_id(&self) -> Option<Bytes32> {
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

    pub fn get_quality_string(&self, plot_id: &Bytes32) -> Result<Bytes32, Error> {
        Ok(Bytes32::new(
            validate_proof(
                &plot_id.to_sized_bytes(),
                self.size,
                &self.challenge.to_bytes(),
                &self.proof.to_bytes(),
            )?
            .to_bytes(),
        ))
    }

    pub fn hash(&self) -> Vec<u8> {
        let mut hasher: Sha256 = Sha256::new();
        hasher.update(self.to_bytes());
        hasher.finalize().to_vec()
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

pub fn verify_and_get_quality_string(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: &Bytes32,
    signage_point: &Bytes32,
) -> Option<Bytes32> {
    if pos.pool_public_key.is_none() && pos.pool_contract_puzzle_hash.is_none() {
        warn!("Failed to Verify ProofOfSpace: null value for pool_public_key and pool_contract_puzzle_hash");
        return None;
    }
    if pos.pool_public_key.is_some() && pos.pool_contract_puzzle_hash.is_some() {
        warn!("Failed to Verify ProofOfSpace: Non Null value for both for pool_public_key and pool_contract_puzzle_hash");
        return None;
    }
    if pos.size < constants.min_plot_size {
        warn!("Failed to Verify ProofOfSpace: Plot failed MIN_PLOT_SIZE");
        return None;
    }
    if pos.size > constants.max_plot_size {
        warn!("Failed to Verify ProofOfSpace: Plot failed MAX_PLOT_SIZE");
        return None;
    }
    if let Some(plot_id) = pos.get_plot_id() {
        if pos.challenge
            != calculate_pos_challenge(&plot_id, original_challenge_hash, signage_point)
        {
            warn!("Failed to Verify ProofOfSpace: New challenge is not challenge");
            return None;
        }
        if !passes_plot_filter(constants, &plot_id, original_challenge_hash, signage_point) {
            warn!("Failed to Verify ProofOfSpace: Plot Failed to Pass Filter");
            return None;
        }
        pos.get_quality_string(&plot_id).ok()
    } else {
        None
    }
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
