use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::vdf_info::VdfInfo;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RewardChainSubSlot {
    pub end_of_slot_vdf: VdfInfo,
    pub challenge_chain_sub_slot_hash: Bytes32,
    pub infused_challenge_chain_sub_slot_hash: Option<Bytes32>,
    pub deficit: u8,
}
