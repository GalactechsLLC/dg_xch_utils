use crate::types::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::types::blockchain::full_block::FullBlock;
use crate::types::blockchain::peer_info::TimestampedPeerInfo;
use crate::types::blockchain::sized_bytes::Bytes32;
use crate::types::blockchain::spend_bundle::SpendBundle;
use crate::types::blockchain::unfinished_block::UnfinishedBlock;
use crate::types::blockchain::vdf_info::VdfInfo;
use crate::types::blockchain::vdf_proof::VdfProof;
use crate::types::blockchain::weight_proof::WeightProof;

pub struct NewPeak {
    pub header_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
    pub fork_point_with_previous_peak: u32,
    pub unfinished_reward_block_hash: Bytes32,
}

pub struct NewTransaction {
    pub transaction_id: Bytes32,
    pub cost: u64,
    pub fees: u64,
}

pub struct RequestTransaction {
    pub transaction_id: Bytes32,
}

pub struct RespondTransaction {
    pub transaction: SpendBundle,
}

pub struct RequestProofOfWeight {
    pub total_number_of_blocks: u32,
    pub vtip: Bytes32,
}

pub struct RespondProofOfWeight {
    pub wp: WeightProof,
    pub tip: Bytes32,
}

pub struct RequestBlock {
    pub height: u32,
    pub include_transaction_block: bool,
}

pub struct RejectBlock {
    pub height: u32,
}

pub struct RequestBlocks {
    pub start_height: u32,
    pub end_height: u32,
    pub include_transaction_block: bool,
}

pub struct RespondBlocks {
    pub start_height: u32,
    pub end_height: u32,
    pub blocks: Vec<FullBlock>,
}

pub struct RejectBlocks {
    pub start_height: u32,
    pub end_height: u32,
}

pub struct RespondBlock {
    pub block: FullBlock,
}

pub struct NewUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32,
}

pub struct RequestUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32,
}

pub struct RespondUnfinishedBlock {
    pub unfinished_block: UnfinishedBlock,
}

pub struct NewSignagePointOrEndOfSubSlot {
    pub prev_challenge_hash: Option<Bytes32>,
    pub challenge_hash: Bytes32,
    pub index_from_challenge: u8,
    pub last_rc_infusion: Bytes32,
}

pub struct RequestSignagePointOrEndOfSubSlot {
    pub challenge_hash: Bytes32,
    pub index_from_challenge: u8,
    pub last_rc_infusion: Bytes32,
}

pub struct RespondSignagePoint {
    pub index_from_challenge: u8,
    pub challenge_chain_vdf: VdfInfo,
    pub challenge_chain_proof: VdfProof,
    pub reward_chain_vdf: VdfInfo,
    pub reward_chain_proof: VdfProof,
}

pub struct RespondEndOfSubSlot {
    pub end_of_slot_bundle: EndOfSubSlotBundle,
}

pub struct RequestMempoolTransactions {
    pub filter: Vec<u8>,
}

pub struct NewCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
}

pub struct RequestCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
}

pub struct RespondCompactVDF {
    pub height: u32,
    pub header_hash: Bytes32,
    pub field_vdf: u8,
    pub vdf_info: VdfInfo,
    pub vdf_proof: VdfProof,
}

pub struct RequestPeers {}

pub struct RespondPeers {
    pub peer_list: Vec<TimestampedPeerInfo>,
}
