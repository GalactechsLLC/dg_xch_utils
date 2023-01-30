use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Sync {
    pub sync_mode: bool,
    pub synced: bool,
    pub sync_tip_height: u32,
    pub sync_progress_height: u32,
}
