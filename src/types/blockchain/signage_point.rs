use crate::types::blockchain::vdf_info::VdfInfo;
use crate::types::blockchain::vdf_proof::VdfProof;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignagePoint {
    pub cc_vdf: VdfInfo,
    pub cc_proof: VdfProof,
    pub rc_vdf: VdfInfo,
    pub rc_proof: VdfProof,
}
