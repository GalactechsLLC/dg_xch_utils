use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionPeer {
    pub peer: String,
    pub error: String,
    pub status: u32,
}
