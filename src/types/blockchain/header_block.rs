use crate::types::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::types::blockchain::foliage::Foliage;
use crate::types::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::types::blockchain::reward_chain_block::RewardChainBlock;
use crate::types::blockchain::transactions_info::TransactionsInfo;
use crate::types::blockchain::vdf_proof::VdfProof;

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
