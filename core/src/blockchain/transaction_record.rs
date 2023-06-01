use crate::blockchain::coin::Coin;
use crate::blockchain::spend_bundle::SpendBundle;
use crate::blockchain::transaction_peer::TransactionPeer;
use crate::blockchain::wallet_type::WalletType;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionRecord {
    pub confirmed_at_height: u32,
    pub sent: u32,
    pub wallet_id: u32,
    #[serde(alias = "type")]
    pub wallet_type: WalletType,
    pub created_at_time: u64,
    pub amount: u64,
    pub fee_amount: u64,
    pub to_puzzle_hash: String,
    pub trade_id: u64,
    pub name: u64,
    pub confirmed: bool,
    pub spend_bundle: SpendBundle,
    pub additions: Vec<Coin>,
    pub removals: Vec<Coin>,
    pub sent_to: Vec<TransactionPeer>,
}
