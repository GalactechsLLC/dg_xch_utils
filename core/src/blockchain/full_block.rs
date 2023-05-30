use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::subslot_bundle::SubSlotBundle;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FullBlock {
    pub challenge_chain_ip_proof: VdfProof,
    pub challenge_chain_sp_proof: VdfProof,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
    pub reward_chain_ip_proof: VdfProof,
    pub reward_chain_sp_proof: Option<VdfProof>,
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_generator: Option<String>,
    pub transactions_generator_ref_list: Vec<u32>,
    pub finished_sub_slots: Vec<SubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
    pub transactions_info: Option<TransactionsInfo>,
}
