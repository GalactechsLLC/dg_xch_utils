use crate::blockchain::vdf_info::VdfInfo;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct InfusedChallengeChainSubSlot {
    pub infused_challenge_chain_end_of_slot_vdf: VdfInfo,
}
