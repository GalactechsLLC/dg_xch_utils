use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct Sync {
    pub sync_mode: bool,
    pub synced: bool,
    pub sync_tip_height: u32,
    pub sync_progress_height: u32,
}
