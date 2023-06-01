use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FoliageBlockData {
    pub extension_data: Bytes32,
    pub farmer_reward_puzzle_hash: Bytes32,
    pub unfinished_reward_block_hash: Bytes32,
    pub pool_signature: Option<Bytes96>,
    pub pool_target: PoolTarget,
}
