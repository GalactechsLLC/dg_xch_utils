use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::sub_epoch_summary::SubEpochSummary;
use crate::blockchain::vdf_output::VdfOutput;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct BlockRecord {
    pub header_hash: Bytes32,
    pub prev_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
    pub total_iters: u128,
    pub signage_point_index: u8,
    pub challenge_vdf_output: VdfOutput,
    pub infused_challenge_vdf_output: Option<VdfOutput>,
    pub reward_infusion_new_challenge: Bytes32,
    pub challenge_block_info_hash: Bytes32,
    pub sub_slot_iters: u64,
    pub pool_puzzle_hash: Bytes32,
    pub farmer_puzzle_hash: Bytes32,
    pub required_iters: u64,
    pub deficit: u8,
    pub overflow: bool,
    pub prev_transaction_block_height: u32,
    pub timestamp: Option<u64>,
    pub prev_transaction_block_hash: Option<Bytes32>,
    pub fees: Option<u64>,
    pub reward_claims_incorporated: Option<Vec<Coin>>,
    pub finished_challenge_slot_hashes: Option<Vec<Bytes32>>,
    pub finished_infused_challenge_slot_hashes: Option<Vec<Bytes32>>,
    pub finished_reward_slot_hashes: Option<Vec<Bytes32>>,
    pub sub_epoch_summary_included: Option<SubEpochSummary>,
}
