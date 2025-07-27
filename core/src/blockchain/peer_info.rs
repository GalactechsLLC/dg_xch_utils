use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TimestampedPeerInfo {
    pub host: String,
    pub port: u16,
    pub timestamp: u64,
}
