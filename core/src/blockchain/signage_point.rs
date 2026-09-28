use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

/// All four VDF/proof fields are `Option`: the sub-slot-start signage point (index 0)
/// has no signage VDFs of its own and is validated against the sub-slot boundary.
/// Internal store/RPC type, not a standalone network message.
#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignagePoint {
    pub cc_vdf: Option<VdfInfo>,
    pub cc_proof: Option<VdfProof>,
    pub rc_vdf: Option<VdfInfo>,
    pub rc_proof: Option<VdfProof>,
}

impl SignagePoint {
    /// The sub-slot-start signage point (index 0), carrying no signage VDFs.
    #[must_use]
    pub fn sub_slot_start() -> Self {
        Self {
            cc_vdf: None,
            cc_proof: None,
            rc_vdf: None,
            rc_proof: None,
        }
    }

    /// True if this is the sub-slot-start signage point (all VDFs absent).
    #[must_use]
    pub fn is_sub_slot_start(&self) -> bool {
        self.cc_vdf.is_none()
            && self.cc_proof.is_none()
            && self.rc_vdf.is_none()
            && self.rc_proof.is_none()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/blockchain/signage_point/tests.rs"]
mod tests;
