use crate::types::blockchain::sized_bytes::Bytes32;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FoliageTransactionBlock {
    pub additions_root: Bytes32,
    pub filter_hash: Bytes32,
    pub prev_transaction_block_hash: Bytes32,
    pub removals_root: Bytes32,
    pub transactions_info_hash: Bytes32,
    pub timestamp: u64,
}
