use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub conds: Option<SpendBundleConditions>,
}
