use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignagePoint {
    pub cc_vdf: VdfInfo,
    pub cc_proof: VdfProof,
    pub rc_vdf: VdfInfo,
    pub rc_proof: VdfProof,
}
