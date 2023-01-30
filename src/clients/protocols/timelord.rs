use crate::types::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::types::blockchain::foliage::Foliage;
use crate::types::blockchain::reward_chain_block::RewardChainBlock;
use crate::types::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::sub_epoch_summary::SubEpochSummary;
use crate::types::blockchain::vdf_info::VdfInfo;
use crate::types::blockchain::vdf_proof::VdfProof;

pub struct NewPeakTimelord {
    pub reward_chain_block: RewardChainBlock,
    pub difficulty: u64,
    pub deficit: u8,
    pub sub_slot_iters: u64,
    pub sub_epoch_summary: Option<SubEpochSummary>,
    pub previous_reward_challenges: Vec<(Bytes32, u128)>,
    pub last_challenge_sb_or_eos_total_iters: u128,
    pub passes_ses_height_but_not_yet_included: bool,
}

pub struct NewUnfinishedBlockTimelord {
    pub reward_chain_block: RewardChainBlockUnfinished,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub foliage: Foliage,
    pub sub_epoch_summary: Option<SubEpochSummary>,
    pub rc_prev: Bytes32,
}

pub struct NewInfusionPointVDF {
    pub unfinished_reward_hash: Bytes32,
    pub challenge_chain_ip_vdf: VdfInfo,
    pub challenge_chain_ip_proof: VdfProof,
    pub reward_chain_ip_vdf: VdfInfo,
    pub reward_chain_ip_proof: VdfProof,
    pub infused_challenge_chain_ip_vdf: Option<VdfInfo>,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
}

pub struct NewSignagePointVDF {
    pub index_from_challenge: u8,
    pub challenge_chain_sp_vdf: VdfInfo,
    pub challenge_chain_sp_proof: VdfProof,
    pub reward_chain_sp_vdf: VdfInfo,
    pub reward_chain_sp_proof: VdfProof,
}

pub struct NewEndOfSubSlotVDF {
    pub end_of_sub_slot_bundle: EndOfSubSlotBundle,
}

pub struct RequestCompactProofOfTime {
    pub new_proof_of_time: VdfInfo,
    pub header_hash: Bytes32,
    pub height: u32,
    pub field_vdf: u8,
}

pub struct RespondCompactProofOfTime {
    pub vdf_info: VdfInfo,
    pub vdf_proof: VdfProof,
    pub header_hash: Bytes32,
    pub height: u32,
    pub field_vdf: u8,
}
