use crate::blockchain::foliage_block_data::FoliageBlockData;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Foliage {
    pub prev_block_hash: Bytes32,
    pub reward_block_hash: Bytes32,
    pub foliage_block_data: FoliageBlockData,
    pub foliage_block_data_signature: Bytes96,
    pub foliage_transaction_block_hash: Option<Bytes32>,
    pub foliage_transaction_block_signature: Option<Bytes96>,
}
