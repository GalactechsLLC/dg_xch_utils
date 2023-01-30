use crate::proof_of_space::util::bytes_to_u64;
use crate::types::blockchain::pool_target::PoolTarget;
use crate::types::blockchain::proof_of_space::ProofOfSpace;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::types::ChiaSerialize;
use serde::{Deserialize, Serialize};
use std::io::Error;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePoint {
    pub challenge_hash: Bytes32,
    pub challenge_chain_sp: Bytes32,
    pub reward_chain_sp: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub signage_point_index: u8,
}
impl ChiaSerialize for NewSignagePoint {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.challenge_chain_sp.to_sized_bytes());
        bytes.extend(self.reward_chain_sp.to_sized_bytes());
        bytes.extend(self.difficulty.to_be_bytes());
        bytes.extend(self.sub_slot_iters.to_be_bytes());
        bytes.extend(self.signage_point_index.to_be_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let (challenge_hash, rest) = bytes.split_at(32);
        let (challenge_chain_sp, rest) = rest.split_at(32);
        let (reward_chain_sp, rest) = rest.split_at(32);
        let (difficulty, rest) = rest.split_at(8);
        let (sub_slot_iters, signage_point_index) = rest.split_at(8);
        Ok(Self {
            challenge_hash: Bytes32::from(challenge_hash),
            challenge_chain_sp: Bytes32::from(challenge_chain_sp),
            reward_chain_sp: Bytes32::from(reward_chain_sp),
            difficulty: bytes_to_u64(difficulty),
            sub_slot_iters: bytes_to_u64(sub_slot_iters),
            signage_point_index: signage_point_index[0],
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct DeclareProofOfSpace {
    pub challenge_hash: Bytes32,
    pub challenge_chain_sp: Bytes32,
    pub signage_point_index: u8,
    pub reward_chain_sp: Bytes32,
    pub proof_of_space: ProofOfSpace,
    pub challenge_chain_sp_signature: Bytes96,
    pub reward_chain_sp_signature: Bytes96,
    pub farmer_puzzle_hash: Bytes32,
    pub pool_target: Option<PoolTarget>,
    pub pool_signature: Option<Bytes96>,
}
impl ChiaSerialize for DeclareProofOfSpace {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.challenge_hash.to_sized_bytes());
        bytes.extend(self.challenge_chain_sp.to_sized_bytes());
        bytes.extend(self.signage_point_index.to_be_bytes());
        bytes.extend(self.reward_chain_sp.to_sized_bytes());
        bytes.extend(self.proof_of_space.to_bytes());
        bytes.extend(self.challenge_chain_sp_signature.to_sized_bytes());
        bytes.extend(self.reward_chain_sp_signature.to_sized_bytes());
        bytes.extend(self.farmer_puzzle_hash.to_sized_bytes());
        match &self.pool_target {
            None => {
                bytes.push(0);
            }
            Some(d) => {
                bytes.push(1);
                bytes.extend(d.to_bytes());
            }
        }
        match &self.pool_signature {
            None => {
                bytes.push(0);
            }
            Some(d) => {
                bytes.push(1);
                bytes.extend(d.to_sized_bytes());
            }
        }
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let (challenge_hash, rest) = bytes.split_at(32);
        let (challenge_chain_sp, rest) = rest.split_at(32);
        let (signage_point_index, rest) = rest.split_at(1);
        let (reward_chain_sp, rest) = rest.split_at(32);
        let pos = ProofOfSpace::from_bytes(rest)?;
        let pos_len = pos.to_bytes().len();
        let (_, rest) = rest.split_at(pos_len);
        let (challenge_chain_sp_signature, rest) = rest.split_at(96);
        let (reward_chain_sp_signature, rest) = rest.split_at(96);
        let (farmer_puzzle_hash, rest) = rest.split_at(32);
        let (pool_target_exists, mut rest) = rest.split_at(1);
        let pool_tgt;
        if pool_target_exists[0] == 1 {
            let pool_target = PoolTarget::from_bytes(rest)?;
            let pool_target_len = pool_target.to_bytes().len();
            let (_, r) = rest.split_at(pool_target_len);
            rest = r;
            pool_tgt = Some(pool_target);
        } else {
            pool_tgt = None;
        }
        let (pool_signature_exists, rest) = rest.split_at(1);
        let pool_sig = if pool_signature_exists[0] == 1 {
            let (pool_signature, _) = rest.split_at(96);
            Some(Bytes96::from(pool_signature))
        } else {
            None
        };
        Ok(Self {
            challenge_hash: Bytes32::from(challenge_hash),
            challenge_chain_sp: Bytes32::from(challenge_chain_sp),
            signage_point_index: signage_point_index[0],
            reward_chain_sp: Bytes32::from(reward_chain_sp),
            proof_of_space: pos,
            challenge_chain_sp_signature: Bytes96::from(challenge_chain_sp_signature),
            reward_chain_sp_signature: Bytes96::from(reward_chain_sp_signature),
            farmer_puzzle_hash: Bytes32::from(farmer_puzzle_hash),
            pool_target: pool_tgt,
            pool_signature: pool_sig,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignedValues {
    pub quality_string: Bytes32,
    pub foliage_block_data_hash: Bytes32,
    pub foliage_transaction_block_hash: Bytes32,
}
impl ChiaSerialize for RequestSignedValues {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.quality_string.to_sized_bytes());
        bytes.extend(self.foliage_block_data_hash.to_sized_bytes());
        bytes.extend(self.foliage_transaction_block_hash.to_sized_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let (quality_string, rest) = bytes.split_at(32);
        let (foliage_block_data_hash, rest) = rest.split_at(32);
        let (foliage_transaction_block_hash, _) = rest.split_at(32);
        Ok(Self {
            quality_string: Bytes32::from(quality_string),
            foliage_block_data_hash: Bytes32::from(foliage_block_data_hash),
            foliage_transaction_block_hash: Bytes32::from(foliage_transaction_block_hash),
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FarmingInfo {
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub timestamp: u64,
    pub passed: u32,
    pub proofs: u32,
    pub total_plots: u32,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignedValues {
    pub quality_string: Bytes32,
    pub foliage_block_data_signature: Bytes96,
    pub foliage_transaction_block_signature: Bytes96,
}
impl ChiaSerialize for SignedValues {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.quality_string.to_sized_bytes());
        bytes.extend(self.foliage_block_data_signature.to_sized_bytes());
        bytes.extend(self.foliage_transaction_block_signature.to_sized_bytes());
        bytes
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self, Error>
    where
        Self: Sized,
    {
        let (quality_string, rest) = bytes.split_at(32);
        let (foliage_block_data_signature, rest) = rest.split_at(96);
        let (foliage_transaction_block_signature, _) = rest.split_at(96);
        Ok(Self {
            quality_string: Bytes32::from(quality_string),
            foliage_block_data_signature: Bytes96::from(foliage_block_data_signature),
            foliage_transaction_block_signature: Bytes96::from(foliage_transaction_block_signature),
        })
    }
}
