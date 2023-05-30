use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::vdf_proof::VdfProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HeaderBlock {
    pub finished_sub_slots: Vec<EndOfSubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
    pub challenge_chain_sp_proof: Option<VdfProof>,
    pub challenge_chain_ip_proof: VdfProof,
    pub reward_chain_sp_proof: Option<VdfProof>,
    pub reward_chain_ip_proof: VdfProof,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_filter: Vec<u8>,
    pub transactions_info: Option<TransactionsInfo>,
}
