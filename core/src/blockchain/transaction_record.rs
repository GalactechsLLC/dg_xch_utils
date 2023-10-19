use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::spend_bundle::SpendBundle;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionRecord {
    pub confirmed_at_height: u32,
    pub created_at_time: u64,
    pub to_puzzle_hash: Bytes32,
    pub amount: u64,
    pub fee_amount: u64,
    pub confirmed: bool,
    pub sent: u32,
    pub spend_bundle: Option<SpendBundle>,
    pub additions: Vec<Coin>,
    pub removals: Vec<Coin>,
    pub wallet_id: u32,
    pub sent_to: Vec<(String, u8, Option<String>)>,
    pub trade_id: Option<Bytes32>,
    #[serde(alias = "type")]
    pub transaction_type: u32,
    pub name: Bytes32,
    pub memos: Vec<(Bytes32, Vec<Vec<u8>>)>,
}

pub enum TransactionType {
    IncomingTx = 0,
    OutgoingTx = 1,
    CoinbaseReward = 2,
    FeeReward = 3,
    IncomingTrade = 4,
    OutgoingTrade = 5,
}
