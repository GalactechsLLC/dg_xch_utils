use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WalletBalance {
    pub wallet_id: u32,
    pub pending_coin_removal_count: u32,
    pub unspent_coin_count: u32,
    pub confirmed_wallet_balance: u64,
    pub max_send_amount: u64,
    pub pending_change: u64,
    pub spendable_balance: u64,
    pub unconfirmed_wallet_balance: u64,
}
