use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TransactionPeer {
    pub peer: String,
    pub error: String,
    pub status: u32,
}
