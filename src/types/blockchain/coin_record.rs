use crate::types::blockchain::coin::Coin;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoinRecord {
    pub coin: Coin,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub timestamp: u64,
    pub coinbase: bool,
    pub spent: bool,
}
