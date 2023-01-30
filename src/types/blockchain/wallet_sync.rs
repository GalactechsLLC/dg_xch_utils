use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct WalletSync {
    pub genesis_initialized: bool,
    pub synced: bool,
    pub syncing: bool,
}
