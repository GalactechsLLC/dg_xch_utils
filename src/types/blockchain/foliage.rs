use crate::types::blockchain::foliage_block_data::FoliageBlockData;
use crate::types::blockchain::sized_bytes::{Bytes32, Bytes96};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Foliage {
    pub foliage_transaction_block_hash: Option<Bytes32>,
    pub prev_block_hash: Bytes32,
    pub reward_block_hash: Bytes32,
    pub foliage_block_data_signature: Bytes96,
    pub foliage_transaction_block_signature: Option<Bytes96>,
    pub foliage_block_data: FoliageBlockData,
}
