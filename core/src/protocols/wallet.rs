use crate::blockchain::coin::Coin;
use crate::blockchain::header_block::HeaderBlock;
use crate::blockchain::sized_bytes::Bytes32;
use crate::blockchain::spend_bundle::SpendBundle;
use crate::clvm::program::SerializedProgram;
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPuzzleSolution {
    pub coin_name: Bytes32,
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PuzzleSolutionResponse {
    pub coin_name: Bytes32,
    pub height: u32,
    pub puzzle: SerializedProgram,
    pub solution: SerializedProgram,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPuzzleSolution {
    pub response: PuzzleSolutionResponse,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectPuzzleSolution {
    pub coin_name: Bytes32,
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SendTransaction {
    pub transaction: SpendBundle,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct TransactionAck {
    pub txid: Bytes32,
    pub status: u8,
    pub error: Option<String>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewPeakWallet {
    pub header_hash: Bytes32,
    pub height: u32,
    pub weight: u128,
    pub fork_point_with_previous_peak: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlockHeader {
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlockHeader {
    pub header_block: HeaderBlock,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectHeaderRequest {
    pub height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestRemovals {
    pub height: u32,
    pub header_hash: Bytes32,
    pub coin_names: Option<Vec<Bytes32>>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondRemovals {
    pub height: u32,
    pub header_hash: Bytes32,
    pub coins: Vec<(Bytes32, Option<Coin>)>,
    pub proofs: Option<Vec<(Bytes32, Vec<u8>)>>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectRemovalsRequest {
    pub height: u32,
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestAdditions {
    pub height: u32,
    pub header_hash: Option<Bytes32>,
    pub puzzle_hashes: Option<Vec<Bytes32>>,
}

pub type Proofs = Option<Vec<(Bytes32, Vec<u8>, Option<Vec<u8>>)>>;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondAdditions {
    pub height: u32,
    pub header_hash: Bytes32,
    pub coins: Vec<(Bytes32, Vec<Coin>)>,
    pub proofs: Proofs,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectAdditionsRequest {
    pub height: u32,
    pub header_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondBlockHeaders {
    pub start_height: u32,
    pub end_height: u32,
    pub header_blocks: Vec<HeaderBlock>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectBlockHeaders {
    pub start_height: u32,
    pub end_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestBlockHeaders {
    pub start_height: u32,
    pub end_height: u32,
    pub return_filter: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestHeaderBlocks {
    pub start_height: u32,
    pub end_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RejectHeaderBlocks {
    pub start_height: u32,
    pub end_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondHeaderBlocks {
    pub start_height: u32,
    pub end_height: u32,
    pub header_blocks: Vec<HeaderBlock>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RegisterForPhUpdates {
    pub puzzle_hashes: Vec<Bytes32>,
    pub min_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CoinState {
    pub coin: Coin,
    pub spent_height: Option<u32>,
    pub created_height: Option<u32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RegisterForCoinUpdates {
    pub coin_ids: Vec<Bytes32>,
    pub min_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondToCoinUpdates {
    pub coin_ids: Vec<Bytes32>,
    pub min_height: u32,
    pub coin_states: Vec<CoinState>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct CoinStateUpdate {
    pub height: u32,
    pub fork_height: u32,
    pub peak_hash: Bytes32,
    pub items: Vec<CoinState>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestChildren {
    pub coin_name: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondChildren {
    pub coin_states: Vec<CoinState>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSESInfo {
    pub start_height: u32,
    pub end_height: u32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSESInfo {
    pub reward_chain_hash: Vec<Bytes32>,
    pub heights: Vec<Vec<u32>>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestFeeEstimates {
    pub time_targets: Vec<u64>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondFeeEstimates {
    pub estimates: FeeEstimateGroup,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FeeEstimate {
    pub error: Option<String>,
    pub time_target: u64,
    pub estimated_fee_rate: FeeRate,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FeeEstimateGroup {
    pub error: Option<String>,
    pub estimates: Vec<FeeEstimate>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FeeRate {
    pub mojos_per_clvm_cost: u64,
}
