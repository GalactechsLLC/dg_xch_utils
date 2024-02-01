use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::subslot_bundle::SubSlotBundle;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::program::SerializedProgram;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FullBlock {
    pub finished_sub_slots: Vec<SubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
    pub challenge_chain_sp_proof: VdfProof,
    pub challenge_chain_ip_proof: VdfProof,
    pub reward_chain_sp_proof: Option<VdfProof>,
    pub reward_chain_ip_proof: VdfProof,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_info: Option<TransactionsInfo>,
    pub transactions_generator: Option<SerializedProgram>,
    pub transactions_generator_ref_list: Vec<u32>,
}
