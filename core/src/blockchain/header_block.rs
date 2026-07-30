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
    /// chia_rs `HeaderBlock::prev_header_hash` — the foliage's previous-block hash.
    #[must_use]
    pub fn prev_header_hash(&self) -> Bytes32 {
        self.foliage.prev_block_hash
    }

    /// chia_rs `HeaderBlock::header_hash` — the hash of the foliage.
    ///
    /// # Errors
    /// Returns an error if the foliage fails to serialize.
    pub fn header_hash(&self) -> Result<Bytes32, std::io::Error> {
        Ok(hash_256(self.foliage.to_bytes(ChiaProtocolVersion::default())?).into())
    }

    /// chia_rs `HeaderBlock::height` — the reward-chain block height.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.reward_chain_block.height
    }

    /// chia_rs `HeaderBlock::weight` — the reward-chain block weight.
    #[must_use]
    pub fn weight(&self) -> u128 {
        self.reward_chain_block.weight
    }

    /// chia_rs `HeaderBlock::total_iters` — the reward-chain block total iterations.
    #[must_use]
    pub fn total_iters(&self) -> u128 {
        self.reward_chain_block.total_iters
    }

    /// chia_rs `HeaderBlock::first_in_sub_slot` — true when this block starts a new sub-slot.
    #[must_use]
    pub fn first_in_sub_slot(&self) -> bool {
        !self.finished_sub_slots.is_empty()
    }
}
