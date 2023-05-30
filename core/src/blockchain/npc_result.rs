use crate::blockchain::npc::NPC;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub conds: Option<SpendBundleConditions>,
    pub cost: u64,
}
