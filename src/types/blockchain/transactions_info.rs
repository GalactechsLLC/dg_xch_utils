use crate::types::blockchain::coin::Coin;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TransactionsInfo {
    pub aggregated_signature: String,
    pub generator_refs_root: String,
    pub generator_root: String,
    pub cost: u64,
    pub fees: u64,
    pub reward_claims_incorporated: Vec<Coin>,
}
