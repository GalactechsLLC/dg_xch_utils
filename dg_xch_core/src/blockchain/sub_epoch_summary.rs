use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubEpochSummary {
    pub prev_subepoch_summary_hash: Bytes32,
    pub reward_chain_hash: Bytes32,
    pub num_blocks_overflow: u8,
    pub new_difficulty: Option<u64>,
    pub new_sub_slot_iters: Option<u64>,
}
