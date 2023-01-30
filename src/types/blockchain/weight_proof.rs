use crate::types::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::types::blockchain::header_block::HeaderBlock;
use crate::types::blockchain::proof_of_space::ProofOfSpace;
use crate::types::blockchain::reward_chain_block::RewardChainBlock;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::vdf_info::VdfInfo;
use crate::types::blockchain::vdf_proof::VdfProof;

pub struct SubEpochData {
    pub reward_chain_hash: Bytes32,
    pub num_blocks_overflow: u8,
    pub new_sub_slot_iters: Option<u64>,
    pub new_difficulty: Option<u64>,
}

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

// def is_challenge(self) -> bool:
// if self.proof_of_space is not None:
// return True
// return False
//
// def is_end_of_slot(self) -> bool:
// if self.cc_slot_end_info is not None:
// return True
// return False

pub struct SubEpochChallengeSegment {
    pub sub_epoch_n: u32,
    pub sub_slots: Vec<SubSlotData>,
    pub rc_slot_end_info: Option<VdfInfo>,
}

pub struct SubEpochSegments {
    pub challenge_segments: Vec<SubEpochChallengeSegment>,
}

pub struct RecentChainData {
    pub recent_chain_data: Vec<HeaderBlock>,
}

pub struct ProofBlockHeader {
    pub finished_sub_slots: Vec<EndOfSubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
}

pub struct WeightProof {
    pub sub_epochs: Vec<SubEpochData>,
    pub sub_epoch_segments: Vec<SubEpochChallengeSegment>,
    pub recent_chain_data: Vec<HeaderBlock>,
}
