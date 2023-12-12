use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::foliage::Foliage;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::sub_epoch_summary::SubEpochSummary;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
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

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewUnfinishedBlockTimelord {
    pub reward_chain_block: RewardChainBlockUnfinished,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub foliage: Foliage,
    pub sub_epoch_summary: Option<SubEpochSummary>,
    pub rc_prev: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewInfusionPointVDF {
    pub unfinished_reward_hash: Bytes32,
    pub challenge_chain_ip_vdf: VdfInfo,
    pub challenge_chain_ip_proof: VdfProof,
    pub reward_chain_ip_vdf: VdfInfo,
    pub reward_chain_ip_proof: VdfProof,
    pub infused_challenge_chain_ip_vdf: Option<VdfInfo>,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointVDF {
    pub index_from_challenge: u8,
    pub challenge_chain_sp_vdf: VdfInfo,
    pub challenge_chain_sp_proof: VdfProof,
    pub reward_chain_sp_vdf: VdfInfo,
    pub reward_chain_sp_proof: VdfProof,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewEndOfSubSlotVDF {
    pub end_of_sub_slot_bundle: EndOfSubSlotBundle,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestCompactProofOfTime {
    pub new_proof_of_time: VdfInfo,
    pub header_hash: Bytes32,
    pub height: u32,
    pub field_vdf: u8,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondCompactProofOfTime {
    pub vdf_info: VdfInfo,
    pub vdf_proof: VdfProof,
    pub header_hash: Bytes32,
    pub height: u32,
    pub field_vdf: u8,
}
