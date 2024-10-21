use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::header_block::HeaderBlock;
use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubEpochData {
    pub reward_chain_hash: Bytes32,
    pub num_blocks_overflow: u8,
    pub new_sub_slot_iters: Option<u64>,
    pub new_difficulty: Option<u64>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubSlotData {
    pub proof_of_space: Option<ProofOfSpace>,
    pub cc_signage_point: Option<VdfProof>,
    pub cc_infusion_point: Option<VdfProof>,
    pub icc_infusion_point: Option<VdfProof>,
    pub cc_sp_vdf_info: Option<VdfInfo>,
    pub signage_point_index: Option<u8>,
    pub cc_slot_end: Option<VdfProof>,
    pub icc_slot_end: Option<VdfProof>,
    pub cc_slot_end_info: Option<VdfInfo>,
    pub icc_slot_end_info: Option<VdfInfo>,
    pub cc_ip_vdf_info: Option<VdfInfo>,
    pub icc_ip_vdf_info: Option<VdfInfo>,
    pub total_iters: Option<u128>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubEpochChallengeSegment {
    pub sub_epoch_n: u32,
    pub sub_slots: Vec<SubSlotData>,
    pub rc_slot_end_info: Option<VdfInfo>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SubEpochSegments {
    pub challenge_segments: Vec<SubEpochChallengeSegment>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RecentChainData {
    pub recent_chain_data: Vec<HeaderBlock>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofBlockHeader {
    pub finished_sub_slots: Vec<EndOfSubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WeightProof {
    pub sub_epochs: Vec<SubEpochData>,
    pub sub_epoch_segments: Vec<SubEpochChallengeSegment>,
    pub recent_chain_data: Vec<HeaderBlock>,
}
