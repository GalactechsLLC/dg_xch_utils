use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::blockchain::subslot_bundle::SubSlotBundle;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct UnfinishedBlock {
    pub challenge_chain_sp_proof: Option<VdfProof>,
    pub reward_chain_sp_proof: Option<VdfProof>,
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_filter: String,
    pub finished_sub_slots: Vec<SubSlotBundle>,
    pub reward_chain_block: RewardChainBlockUnfinished,
    pub transactions_info: Option<TransactionsInfo>,
    pub transactions_generator: Option<String>,
    pub transactions_generator_ref_list: Option<Vec<u32>>,
}
