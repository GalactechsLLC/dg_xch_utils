use crate::blockchain::coin::Coin;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use crate::blockchain::sized_bytes::Bytes32;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CoinRecord {
    pub coin: Coin,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub timestamp: u64,
    pub coinbase: bool,
    pub spent: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HintedCoinRecord { //Not Standard Protocol
    pub coin: Coin,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub timestamp: u64,
    pub coinbase: bool,
    pub spent: bool,
    pub hint: Option<Bytes32>,
}