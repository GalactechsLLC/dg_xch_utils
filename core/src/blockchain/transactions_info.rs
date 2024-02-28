use crate::blockchain::coin::Coin;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionsInfo {
    pub generator_root: String,
    pub generator_refs_root: String,
    pub aggregated_signature: String,
    pub fees: u64,
    pub cost: u64,
    pub reward_claims_incorporated: Vec<Coin>,
}
