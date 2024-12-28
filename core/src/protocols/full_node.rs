use crate::blockchain::coin_record::CoinRecord;
use crate::blockchain::end_of_subslot_bundle::EndOfSubSlotBundle;
use crate::blockchain::full_block::FullBlock;
use crate::blockchain::peer_info::TimestampedPeerInfo;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::spend_bundle::SpendBundle;
use crate::blockchain::unfinished_block::UnfinishedBlock;
use crate::blockchain::vdf_info::VdfInfo;
use crate::blockchain::vdf_proof::VdfProof;
use crate::blockchain::weight_proof::WeightProof;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewPeak {
    pub header_hash: Bytes32,                  //Min Version 0.0.34
    pub height: u32,                           //Min Version 0.0.34
    pub weight: u128,                          //Min Version 0.0.34
    pub fork_point_with_previous_peak: u32,    //Min Version 0.0.34
    pub unfinished_reward_block_hash: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewTransaction {
    pub transaction_id: Bytes32, //Min Version 0.0.34
    pub cost: u64,               //Min Version 0.0.34
    pub fees: u64,               //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestTransaction {
    pub transaction_id: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondTransaction {
    pub transaction: SpendBundle, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestProofOfWeight {
    pub total_number_of_blocks: u32, //Min Version 0.0.34
    pub tip: Bytes32,                //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondProofOfWeight {
    pub wp: WeightProof, //Min Version 0.0.34
    pub tip: Bytes32,    //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlock {
    pub height: u32,                     //Min Version 0.0.34
    pub include_transaction_block: bool, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectBlock {
    pub height: u32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct BlockCountMetrics {
    pub compact_blocks: u64,
    pub uncompact_blocks: u64,
    pub hint_count: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlocks {
    pub start_height: u32,               //Min Version 0.0.34
    pub end_height: u32,                 //Min Version 0.0.34
    pub include_transaction_block: bool, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlocks {
    pub start_height: u32,      //Min Version 0.0.34
    pub end_height: u32,        //Min Version 0.0.34
    pub blocks: Vec<FullBlock>, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectBlocks {
    pub start_height: u32, //Min Version 0.0.34
    pub end_height: u32,   //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlock {
    pub block: FullBlock, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestUnfinishedBlock {
    pub unfinished_reward_hash: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewUnfinishedBlock2 {
    pub unfinished_reward_hash: Bytes32, //Min Version 0.0.36
    pub foliage_hash: Option<Bytes32>,   //Min Version 0.0.36
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondUnfinishedBlock {
    pub unfinished_block: UnfinishedBlock, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointOrEndOfSubSlot {
    pub prev_challenge_hash: Option<Bytes32>, //Min Version 0.0.34
    pub challenge_hash: Bytes32,              //Min Version 0.0.34
    pub index_from_challenge: u8,             //Min Version 0.0.34
    pub last_rc_infusion: Bytes32,            //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignagePointOrEndOfSubSlot {
    pub challenge_hash: Bytes32,   //Min Version 0.0.34
    pub index_from_challenge: u8,  //Min Version 0.0.34
    pub last_rc_infusion: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSignagePoint {
    pub index_from_challenge: u8,        //Min Version 0.0.34
    pub challenge_chain_vdf: VdfInfo,    //Min Version 0.0.34
    pub challenge_chain_proof: VdfProof, //Min Version 0.0.34
    pub reward_chain_vdf: VdfInfo,       //Min Version 0.0.34
    pub reward_chain_proof: VdfProof,    //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondEndOfSubSlot {
    pub end_of_slot_bundle: EndOfSubSlotBundle, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestMempoolTransactions {
    pub filter: Vec<u8>, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewCompactVDF {
    pub height: u32,          //Min Version 0.0.34
    pub header_hash: Bytes32, //Min Version 0.0.34
    pub field_vdf: u8,        //Min Version 0.0.34
    pub vdf_info: VdfInfo,    //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestCompactVDF {
    pub height: u32,          //Min Version 0.0.34
    pub header_hash: Bytes32, //Min Version 0.0.34
    pub field_vdf: u8,        //Min Version 0.0.34
    pub vdf_info: VdfInfo,    //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondCompactVDF {
    pub height: u32,          //Min Version 0.0.34
    pub header_hash: Bytes32, //Min Version 0.0.34
    pub field_vdf: u8,        //Min Version 0.0.34
    pub vdf_info: VdfInfo,    //Min Version 0.0.34
    pub vdf_proof: VdfProof,  //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPeers {} //Min Version 0.0.34

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPeers {
    pub peer_list: Vec<TimestampedPeerInfo>, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct FeeEstimate {
    estimates: Vec<u64>,
    target_times: Vec<u64>,
    current_fee_rate: f64,
    mempool_size: u64,
    mempool_fees: u64,
    num_spends: u64,
    mempool_max_size: u64,
    full_node_synced: bool,
    peak_height: u64,
    last_peak_timestamp: u64,
    node_time_utc: u64,
    last_block_cost: u64,
    fees_last_block: Option<u64>,
    fee_rate_last_block: f64,
    last_tx_block_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct BlockRequest {
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct BlocksRequest {
    pub start: u32,
    pub end: u32,
    pub exclude_header_hash: bool,
    pub exclude_reorged: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct AllBlocksRequest {
    pub start: u32,
    pub end: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct BlockRecordByHeightRequest {
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct BlockRecordRequest {
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct BlockRecordsRequest {
    pub start: u32,
    pub end: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct NetworkSpaceRequest {
    pub older_block_header_hash: Bytes32,
    pub newer_block_header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct RecentSignagePointorEOSRequest {
    pub sp_hash: Option<Bytes32>,
    pub challenge_hash: Option<Bytes32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByPuzzleHashRequest {
    pub puzzle_hash: Bytes32,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByPuzzleHashesRequest {
    pub puzzle_hashes: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordByNameRequest {
    pub name: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordByNamesRequest {
    pub names: Vec<String>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByParentIdsRequest {
    pub parent_ids: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByHintRequest {
    pub hint: Bytes32,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct PushTxRequest {
    pub spend_bundle: SpendBundle,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct PuzzleAndSolutionRequest {
    pub coin_id: Bytes32,
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinSpendRequest {
    pub coin_record: CoinRecord,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct MempoolItemByTxIdRequest {
    pub tx_id: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct NetworkSpaceByHeightRequest {
    pub older_block_height: u32,
    pub newer_block_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct AdditionsAndRemovalsRequest {
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct MempoolItemByCoinNameRequest {
    pub coin_name: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct FeeEstimateRequest {
    pub cost: Option<u64>,
    pub spend_bundle: Option<SpendBundle>,
    pub spend_type: Option<String>,
    pub target_times: Vec<u64>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct AdditionsAndRemovalsWithHintRequest {
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct SingletonByLauncherIdRequest {
    pub launcher_id: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByHintsRequest {
    pub hints: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByHintsPaginatedRequest {
    pub hints: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
    pub page_size: Option<u32>,
    pub last_id: Option<Bytes32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct CoinRecordsByPuzzleHashesPaginatedRequest {
    pub puzzle_hashes: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
    pub page_size: Option<u32>,
    pub last_id: Option<Bytes32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct HintsByCoinIdRequest {
    pub coin_ids: Vec<Bytes32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Serialize, Deserialize, Debug)]
pub struct PuzzlesAndSolutionsByNamesRequest {
    pub names: Vec<Bytes32>,
    pub include_spent_coins: Option<bool>,
    pub start_height: Option<u32>,
    pub end_height: Option<u32>,
}
