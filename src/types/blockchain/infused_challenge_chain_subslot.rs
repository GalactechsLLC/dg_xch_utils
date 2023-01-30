use crate::types::blockchain::vdf_info::VdfInfo;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InfusedChallengeChainSubSlot {
    pub infused_challenge_chain_end_of_slot_vdf: VdfInfo,
}
