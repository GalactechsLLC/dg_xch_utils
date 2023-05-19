use dg_xch_core::blockchain::pool_target::PoolTarget;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePoint {
    pub challenge_hash: Bytes32,
    pub challenge_chain_sp: Bytes32,
    pub reward_chain_sp: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub signage_point_index: u8,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignedValues {
    pub quality_string: Bytes32,
    pub foliage_block_data_hash: Bytes32,
    pub foliage_transaction_block_hash: Bytes32,
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
