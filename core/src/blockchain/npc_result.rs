use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub conds: Option<SpendBundleConditions>,
    pub cost: u64,
}
