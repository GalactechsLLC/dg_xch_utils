use crate::blockchain::sized_bytes::Bytes32;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PendingPayment {
    pub puzzle_hash: Bytes32,
    pub amount: u64,
}
