use crate::blockchain::coin::Coin;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::Bytes32;
use crate::clvm::program::Program;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CoinRecord {
    pub coin: Coin,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub coinbase: bool,
    pub timestamp: u64,
    pub spent: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum CatVersion {
    V1,
    V2,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CatCoinRecord {
    pub delegate: CoinRecord,
    pub version: CatVersion,
    pub asset_id: Bytes32,
    pub cat_program: Program,
    pub lineage_proof: Program,
    pub parent_coin_spend: CoinSpend,
}

//Not Standard Protocol
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PaginatedCoinRecord {
    pub coin: Coin,
    pub coin_spend: Option<CoinSpend>,
    pub parent_coin_spend: Option<CoinSpend>,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub timestamp: u64,
    pub coinbase: bool,
    pub spent: bool,
}

//Not Standard Protocol
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HintedCoinRecord {
    pub coin: Coin,
    pub parent_coin_spend: Option<CoinSpend>,
    pub confirmed_block_index: u32,
    pub spent_block_index: u32,
    pub timestamp: u64,
    pub coinbase: bool,
    pub spent: bool,
    pub hint: Option<Bytes32>,
}
