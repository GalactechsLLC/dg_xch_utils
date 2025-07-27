use crate::blockchain::coin::Coin;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionsInfo {
    pub generator_root: Bytes32,
    pub generator_refs_root: Bytes32,
    pub aggregated_signature: Bytes96,
    pub fees: u64,
    pub cost: u64,
    pub reward_claims_incorporated: Vec<Coin>,
}
