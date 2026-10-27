use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::foliage::Foliage;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::header_block::HeaderBlock;
use crate::blockchain::reward_chain_block::RewardChainBlock;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::subslot_bundle::SubSlotBundle;
use crate::blockchain::transactions_info::TransactionsInfo;
use crate::blockchain::unsized_bytes::UnsizedBytes;
use crate::blockchain::vdf_proof::VdfProof;
use crate::clvm::program::SerializedProgram;
use crate::utils::hash_256;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::{ChiaProtocolVersion, ChiaSerialize};
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FullBlock {
    pub finished_sub_slots: Vec<SubSlotBundle>,
    pub reward_chain_block: RewardChainBlock,
    pub challenge_chain_sp_proof: Option<VdfProof>,
    pub challenge_chain_ip_proof: VdfProof,
    pub reward_chain_sp_proof: Option<VdfProof>,
    pub reward_chain_ip_proof: VdfProof,
    pub infused_challenge_chain_ip_proof: Option<VdfProof>,
    pub foliage: Foliage,
    pub foliage_transaction_block: Option<FoliageTransactionBlock>,
    pub transactions_info: Option<TransactionsInfo>,
    pub transactions_generator: Option<SerializedProgram>,
    pub transactions_generator_ref_list: Vec<u32>,
    #[serde(
        default,
        skip_serializing_if = "crate::signatures::extension::AuthorizationExtension::is_none"
    )]
    #[chia_skip_sexp]
    pub authorization: crate::signatures::extension::AuthorizationExtension,
}

impl FullBlock {
    #[must_use]
    pub fn prev_header_hash(&self) -> Bytes32 {
        self.foliage.prev_block_hash
    }

    #[must_use]
    pub fn height(&self) -> u32 {
        self.reward_chain_block.height
    }

    /// The block id, hashed from the foliage.
    ///
    /// # Errors
    /// Returns an error if the foliage fails to serialize.
    pub fn header_hash(&self) -> Result<Bytes32, std::io::Error> {
        Ok(hash_256(self.foliage.to_bytes(ChiaProtocolVersion::default())?).into())
    }

    #[must_use]
    pub fn is_transaction_block(&self) -> bool {
        self.foliage_transaction_block.is_some()
    }

    /// The header view of this block: strips the transaction generator/ref-list and
    /// carries the empty BIP158 filter (the single byte `0x00`).
    #[must_use]
    pub fn get_block_header(&self) -> HeaderBlock {
        HeaderBlock {
            finished_sub_slots: self
                .finished_sub_slots
                .iter()
                .map(|s| EndOfSubSlotBundle {
                    challenge_chain: s.challenge_chain,
                    infused_challenge_chain: s.infused_challenge_chain,
                    reward_chain: s.reward_chain,
                    proofs: s.proofs.clone(),
                })
                .collect(),
            reward_chain_block: self.reward_chain_block.clone(),
            challenge_chain_sp_proof: self.challenge_chain_sp_proof.clone(),
            challenge_chain_ip_proof: self.challenge_chain_ip_proof.clone(),
            reward_chain_sp_proof: self.reward_chain_sp_proof.clone(),
            reward_chain_ip_proof: self.reward_chain_ip_proof.clone(),
            infused_challenge_chain_ip_proof: self.infused_challenge_chain_ip_proof.clone(),
            foliage: self.foliage,
            foliage_transaction_block: self.foliage_transaction_block,
            transactions_filter: UnsizedBytes::new(vec![0]),
            transactions_info: self.transactions_info.clone(),
        }
    }
}
