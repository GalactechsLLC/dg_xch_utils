use crate::blockchain::spend_bundle_conditions::SpendBundleConditions;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
pub struct NPCResult {
    pub error: Option<u16>,
    pub conds: Option<SpendBundleConditions>,
}
