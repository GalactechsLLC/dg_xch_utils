use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WalletSync {
    pub genesis_initialized: bool,
    pub synced: bool,
    pub syncing: bool,
}
