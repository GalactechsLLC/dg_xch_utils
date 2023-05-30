use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::blockchain::vdf_info::VdfInfo;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RewardChainBlock {
    pub pos_ss_cc_challenge_hash: Bytes32,
    pub challenge_chain_sp_signature: Bytes96,
    pub reward_chain_sp_signature: Bytes96,
    pub challenge_chain_sp_vdf: Option<VdfInfo>,
    pub infused_challenge_chain_ip_vdf: Option<VdfInfo>,
    pub challenge_chain_ip_vdf: VdfInfo,
    pub reward_chain_ip_vdf: VdfInfo,
    pub reward_chain_sp_vdf: Option<VdfInfo>,
    pub height: u64,
    pub signage_point_index: u8,
    pub total_iters: u128,
    pub weight: u128,
    pub is_transaction_block: bool,
    pub proof_of_space: ProofOfSpace,
}
