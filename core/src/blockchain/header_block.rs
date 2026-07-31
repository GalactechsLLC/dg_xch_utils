use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::vdf_proof::VdfProof;
use crate::utils::hash_256;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
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
    pub transactions_filter: UnsizedBytes,
    pub transactions_info: Option<TransactionsInfo>,
}

impl HeaderBlock {
    #[must_use]
    pub fn prev_header_hash(&self) -> Bytes32 {
        self.foliage.prev_block_hash
    }

    /// # Errors
    /// Returns an error if the foliage fails to serialize.
    pub fn header_hash(&self) -> Result<Bytes32, std::io::Error> {
        Ok(hash_256(self.foliage.to_bytes(ChiaProtocolVersion::default())?).into())
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.reward_chain_block.height
    }

    #[must_use]
    pub fn weight(&self) -> u128 {
        self.reward_chain_block.weight
    }

    #[must_use]
    pub fn total_iters(&self) -> u128 {
        self.reward_chain_block.total_iters
    }

    #[must_use]
    pub fn first_in_sub_slot(&self) -> bool {
        !self.finished_sub_slots.is_empty()
    }
}
