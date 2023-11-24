use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolDifficulty {
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub pool_contract_puzzle_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HarvesterHandshake {
    pub farmer_public_keys: Vec<Bytes48>,
    pub pool_public_keys: Vec<Bytes48>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointHarvester {
    pub challenge_hash: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub signage_point_index: u8,
    pub sp_hash: Bytes32,
    pub pool_difficulties: Vec<PoolDifficulty>,
    pub filter_prefix_bits: i8,
}
impl Display for NewSignagePointHarvester {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "NewSignagePointHarvester {{")?;
        writeln!(f, "\tchallenge_hash: {:?},", self.challenge_hash)?;
        writeln!(f, "\tdifficulty: {:?},", self.difficulty)?;
        writeln!(f, "\tsub_slot_iters: {:?},", self.sub_slot_iters)?;
        writeln!(f, "\tsignage_point_index: {:?},", self.signage_point_index)?;
        writeln!(f, "\tsp_hash: {:?},", self.sp_hash)?;
        writeln!(f, "\tpool_difficulties: {:?},", self.pool_difficulties)?;
        writeln!(f, "\tfilter_prefix_bits: {:?},", self.filter_prefix_bits)?;
        writeln!(f, "}}")
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewProofOfSpace {
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub plot_identifier: String,
    pub proof: ProofOfSpace,
    pub signage_point_index: u8,
}
#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignatures {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub messages: Vec<Bytes32>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSignatures {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub local_pk: Bytes48,
    pub farmer_pk: Bytes48,
    pub message_signatures: Vec<(Bytes32, Bytes96)>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Plot {
    pub filename: String,
    pub size: u8,
    pub plot_id: Bytes32,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub file_size: u64,
    pub time_modified: u64,
    pub compression_level: Option<u8>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPlots {}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPlots {
    pub plots: Vec<Plot>,
    pub failed_to_open_filenames: Vec<String>,
    pub no_key_filenames: Vec<String>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncIdentifier {
    pub timestamp: u64,
    pub sync_id: u64,
    pub message_id: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncStart {
    pub identifier: PlotSyncIdentifier,
    pub initial: bool,
    pub last_sync_id: u64,
    pub plot_file_count: u32,
    pub harvesting_mode: u8,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPathList {
    pub identifier: PlotSyncIdentifier,
    pub data: Vec<String>,
    pub r#final: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPlotList {
    pub identifier: PlotSyncIdentifier,
    pub data: Vec<Plot>,
    pub r#final: bool,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncDone {
    pub identifier: PlotSyncIdentifier,
    pub duration: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncError {
    pub code: i16,
    pub message: String,
    pub expected_identifier: Option<PlotSyncIdentifier>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncResponse {
    pub identifier: PlotSyncIdentifier,
    pub message_type: i16,
    pub error: Option<PlotSyncError>,
}
