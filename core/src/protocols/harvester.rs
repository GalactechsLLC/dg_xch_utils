use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
#[cfg(feature = "metrics")]
use std::sync::Arc;
#[cfg(feature = "metrics")]
use std::time::Instant;

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

#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericGauge};
#[cfg(feature = "metrics")]
use prometheus::Registry;

#[derive(Debug, Default, Clone)]
pub struct HarvesterState {
    pub og_plot_count: usize,
    pub nft_plot_count: usize,
    pub compressed_plot_count: usize,
    pub invalid_plot_count: usize,
    pub plot_space: u64,
    #[cfg(feature = "metrics")]
    pub metrics: Option<HarvesterMetrics>,
    pub missing_keys: HashSet<Bytes48>,
    pub missing_pool_info: HashMap<Bytes48, Bytes32>,
}

#[cfg(feature = "metrics")]
#[derive(Debug, Clone)]
pub struct HarvesterMetrics {
    pub start_time: Arc<Instant>,
    pub uptime: Option<GenericGauge<AtomicU64>>,
    pub reported_space: Option<GenericGauge<AtomicU64>>,
    pub og_plot_count: Option<GenericGauge<AtomicU64>>,
    pub nft_plot_count: Option<GenericGauge<AtomicU64>>,
    pub compressed_plot_count: Option<GenericGauge<AtomicU64>>,
}
#[cfg(feature = "metrics")]

impl HarvesterMetrics {
    pub fn new(registry: &Registry) -> Self {
        let uptime = GenericGauge::new("harvester_uptime", "Uptime of Harvester").map_or(
            None,
            |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            },
        );
        let reported_space = GenericGauge::new("reported_space", "Reported Space in Bytes").map_or(
            None,
            |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            },
        );
        let og_plot_count = GenericGauge::new("og_plot_count", "OG Plot Count").map_or(
            None,
            |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            },
        );
        let nft_plot_count = GenericGauge::new("nft_plot_count", "NFT Plot Count").map_or(
            None,
            |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            },
        );
        let compressed_plot_count = GenericGauge::new("compressed_plot_count", "OG Plot Count")
            .map_or(None, |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            });
        HarvesterMetrics {
            start_time: Arc::new(Instant::now()),
            uptime,
            reported_space,
            og_plot_count,
            nft_plot_count,
            compressed_plot_count,
        }
    }
}
