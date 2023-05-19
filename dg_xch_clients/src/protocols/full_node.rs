use dg_xch_core::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use dg_xch_core::blockchain::full_block::FullBlock;
use dg_xch_core::blockchain::peer_info::TimestampedPeerInfo;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::blockchain::spend_bundle::SpendBundle;
use dg_xch_core::blockchain::unfinished_block::UnfinishedBlock;
use dg_xch_core::blockchain::vdf_info::VdfInfo;
use dg_xch_core::blockchain::vdf_proof::VdfProof;
use dg_xch_core::blockchain::weight_proof::WeightProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewPeak {
    pub header_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
    pub fork_point_with_previous_peak: u32,
    pub unfinished_reward_block_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewTransaction {
    pub transaction_id: Bytes32,
    pub cost: u64,
    pub fees: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestTransaction {
    pub transaction_id: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondTransaction {
    pub transaction: SpendBundle,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestProofOfWeight {
    pub total_number_of_blocks: u32,
    pub vtip: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondProofOfWeight {
    pub wp: WeightProof,
    pub tip: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlock {
    pub height: u32,
    pub include_transaction_block: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectBlock {
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlocks {
    pub start_height: u32,
    pub end_height: u32,
    pub include_transaction_block: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlocks {
    pub start_height: u32,
    pub end_height: u32,
    pub blocks: Vec<FullBlock>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectBlocks {
    pub start_height: u32,
    pub end_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlock {
    pub block: FullBlock,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondUnfinishedBlock {
    pub unfinished_block: UnfinishedBlock,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointOrEndOfSubSlot {
    pub prev_challenge_hash: Option<Bytes32>,
    pub challenge_hash: Bytes32,
    pub index_from_challenge: u8,
    pub last_rc_infusion: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignagePointOrEndOfSubSlot {
    pub challenge_hash: Bytes32,
    pub index_from_challenge: u8,
    pub last_rc_infusion: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSignagePoint {
    pub index_from_challenge: u8,
    pub challenge_chain_vdf: VdfInfo,
    pub challenge_chain_proof: VdfProof,
    pub reward_chain_vdf: VdfInfo,
    pub reward_chain_proof: VdfProof,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondEndOfSubSlot {
    pub end_of_slot_bundle: EndOfSubSlotBundle,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestMempoolTransactions {
    pub filter: Vec<u8>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
    pub vdf_proof: VdfProof,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPeers {}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPeers {
    pub peer_list: Vec<TimestampedPeerInfo>,
}
