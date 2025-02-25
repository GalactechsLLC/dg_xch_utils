use crate::blockchain::challenge_chain_subslot::ChallengeChainSubSlot;
use crate::blockchain::class_group_element::ClassgroupElement;
use crate::blockchain::foliage_block_data::FoliageBlockData;
use crate::blockchain::foliage_transaction_block::FoliageTransactionBlock;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::blockchain::reward_chain_subslot::RewardChainSubSlot;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::config::PoolWalletConfig;
use crate::protocols::error::RecentErrors;
use crate::protocols::PeerMap;
use blst::min_pk::SecretKey;
use dg_xch_macros::ChiaSerial;
use dg_xch_serialize::ChiaProtocolVersion;

use crate::protocols::shared::Handshake;
#[cfg(feature = "metrics")]
use prometheus::core::{
    AtomicU64, GenericCounter, GenericCounterVec, GenericGauge, GenericGaugeVec,
};
#[cfg(feature = "metrics")]
use prometheus::{Histogram, HistogramOpts, Opts, Registry};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
#[cfg(feature = "metrics")]
use uuid::Uuid;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SPSubSlotSourceData {
    //First in Version 0.0.36
    pub cc_sub_slot: ChallengeChainSubSlot,
    pub rc_sub_slot: RewardChainSubSlot,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SPVDFSourceData {
    //First in Version 0.0.36
    pub cc_vdf: ClassgroupElement,
    pub rc_vdf: ClassgroupElement,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignagePointSourceData {
    //First in Version 0.0.36
    pub sub_slot_data: Option<SPSubSlotSourceData>,
    pub vdf_data: Option<SPVDFSourceData>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePoint {
    pub challenge_hash: Bytes32,                        //Min Version 0.0.34
    pub challenge_chain_sp: Bytes32,                    //Min Version 0.0.34
    pub reward_chain_sp: Bytes32,                       //Min Version 0.0.34
    pub difficulty: u64,                                //Min Version 0.0.34
    pub sub_slot_iters: u64,                            //Min Version 0.0.34
    pub signage_point_index: u8,                        //Min Version 0.0.34
    pub peak_height: u32,                               //Min Version 0.0.35
    pub sp_source_data: Option<SignagePointSourceData>, //Min Version 0.0.36
}
impl dg_xch_serialize::ChiaSerialize for NewSignagePoint {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_chain_sp,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.reward_chain_sp,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.difficulty,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.sub_slot_iters,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.signage_point_index,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_35 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.peak_height,
                version,
            ));
        }
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.sp_source_data,
                version,
            ));
        }
        bytes
    }
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut std::io::Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, std::io::Error>
    where
        Self: Sized,
    {
        let challenge_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let challenge_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let reward_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let difficulty = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let sub_slot_iters = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let signage_point_index = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let peak_height = if version >= ChiaProtocolVersion::Chia0_0_35 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            0u32
        };
        let sp_source_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version).unwrap_or_default()
        } else {
            None
        };
        Ok(Self {
            challenge_hash,
            challenge_chain_sp,
            reward_chain_sp,
            difficulty,
            sub_slot_iters,
            signage_point_index,
            peak_height,
            sp_source_data,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct DeclareProofOfSpace {
    pub challenge_hash: Bytes32,               //Min Version 0.0.34
    pub challenge_chain_sp: Bytes32,           //Min Version 0.0.34
    pub signage_point_index: u8,               //Min Version 0.0.34
    pub reward_chain_sp: Bytes32,              //Min Version 0.0.34
    pub proof_of_space: ProofOfSpace,          //Min Version 0.0.34
    pub challenge_chain_sp_signature: Bytes96, //Min Version 0.0.34
    pub reward_chain_sp_signature: Bytes96,    //Min Version 0.0.34
    pub farmer_puzzle_hash: Bytes32,           //Min Version 0.0.34
    pub pool_target: Option<PoolTarget>,       //Min Version 0.0.34
    pub pool_signature: Option<Bytes96>,       //Min Version 0.0.34
    pub include_signature_source_data: bool,   //Min Version 0.0.36
}

impl dg_xch_serialize::ChiaSerialize for DeclareProofOfSpace {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_chain_sp,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.signage_point_index,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.reward_chain_sp,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.proof_of_space,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_chain_sp_signature,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.reward_chain_sp_signature,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.farmer_puzzle_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.pool_target,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.pool_signature,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.include_signature_source_data,
                version,
            ));
        }
        bytes
    }
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut std::io::Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, std::io::Error>
    where
        Self: Sized,
    {
        let challenge_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let challenge_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let signage_point_index = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let reward_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let proof_of_space = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let challenge_chain_sp_signature =
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let reward_chain_sp_signature =
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let farmer_puzzle_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let pool_target = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let pool_signature = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let include_signature_source_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            false
        };
        Ok(Self {
            challenge_hash,
            challenge_chain_sp,
            signage_point_index,
            reward_chain_sp,
            proof_of_space,
            challenge_chain_sp_signature,
            reward_chain_sp_signature,
            farmer_puzzle_hash,
            pool_target,
            pool_signature,
            include_signature_source_data,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignedValues {
    pub quality_string: Bytes32,                      //Min Version 0.0.34
    pub foliage_block_data_hash: Bytes32,             //Min Version 0.0.34
    pub foliage_transaction_block_hash: Bytes32,      //Min Version 0.0.34
    pub foliage_block_data: Option<FoliageBlockData>, //Min Version 0.0.36
    pub foliage_transaction_block_data: Option<FoliageTransactionBlock>, //Min Version 0.0.36
    pub rc_block_unfinished: Option<RewardChainBlockUnfinished>, //Min Version 0.0.36
}
impl dg_xch_serialize::ChiaSerialize for RequestSignedValues {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.quality_string,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.foliage_block_data_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.foliage_transaction_block_hash,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.foliage_block_data,
                version,
            ));
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.foliage_transaction_block_data,
                version,
            ));
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.rc_block_unfinished,
                version,
            ));
        }
        bytes
    }
    fn from_bytes<T: AsRef<[u8]>>(
        bytes: &mut std::io::Cursor<T>,
        version: ChiaProtocolVersion,
    ) -> Result<Self, std::io::Error>
    where
        Self: Sized,
    {
        let quality_string = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let foliage_block_data_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let foliage_transaction_block_hash =
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let foliage_block_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        let foliage_transaction_block_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        let rc_block_unfinished = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        Ok(Self {
            quality_string,
            foliage_block_data_hash,
            foliage_transaction_block_hash,
            foliage_block_data,
            foliage_transaction_block_data,
            rc_block_unfinished,
        })
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FarmingInfo {
    pub challenge_hash: Bytes32, //Min Version 0.0.34
    pub sp_hash: Bytes32,        //Min Version 0.0.34
    pub timestamp: u64,          //Min Version 0.0.34
    pub passed: u32,             //Min Version 0.0.34
    pub proofs: u32,             //Min Version 0.0.34
    pub total_plots: u32,        //Min Version 0.0.34
    pub lookup_time: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignedValues {
    pub quality_string: Bytes32,                      //Min Version 0.0.34
    pub foliage_block_data_signature: Bytes96,        //Min Version 0.0.34
    pub foliage_transaction_block_signature: Bytes96, //Min Version 0.0.34
}

pub type ProofsMap = Arc<RwLock<HashMap<Bytes32, Vec<(String, ProofOfSpace)>>>>;

#[derive(Clone, Debug)]
pub struct FarmerIdentifier {
    pub plot_identifier: String,
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub peer_node_id: Bytes32,
}

#[derive(Debug, Clone)]
pub struct FarmerPoolState {
    pub points_found_since_start: u64,
    pub points_found_24h: Vec<(Instant, u64)>,
    pub points_acknowledged_since_start: u64,
    pub points_acknowledged_24h: Vec<(Instant, u64)>,
    pub next_farmer_update: Instant,
    pub next_pool_info_update: Instant,
    pub current_points: u64,
    pub current_difficulty: Option<u64>,
    pub pool_config: Option<PoolWalletConfig>,
    pub pool_errors_24h: Vec<(Instant, String)>,
    pub authentication_token_timeout: Option<u8>,
}
impl Default for FarmerPoolState {
    fn default() -> Self {
        Self {
            points_found_since_start: 0,
            points_found_24h: vec![],
            points_acknowledged_since_start: 0,
            points_acknowledged_24h: vec![],
            next_farmer_update: Instant::now(),
            next_pool_info_update: Instant::now(),
            current_points: 0,
            current_difficulty: None,
            pool_config: None,
            pool_errors_24h: vec![],
            authentication_token_timeout: None,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, Copy, Clone)]
pub enum FarmerRunningState {
    #[default]
    Starting,
    NeedsConfig,
    Running,
    Stopped,
    Failed,
    PendingReload,
    Migrating,
}

#[derive(Copy, Clone)]
pub struct MostRecentSignagePoint {
    pub hash: Bytes32,
    pub index: u8,
    pub timestamp: Instant,
}
impl Default for MostRecentSignagePoint {
    fn default() -> Self {
        MostRecentSignagePoint {
            hash: Bytes32::default(),
            index: 0,
            timestamp: Instant::now(),
        }
    }
}

#[cfg(feature = "metrics")]
const PLOT_LOAD_LATENCY_BUCKETS: [f64; 11] = [
    0.01,
    0.05,
    0.1,
    0.25,
    0.5,
    1.0,
    2.5,
    5.0,
    10.0,
    30.0,
    f64::INFINITY,
];
#[cfg(feature = "metrics")]
const LATENCY_BUCKETS: [f64; 11] = [
    0.01,
    0.05,
    0.1,
    0.25,
    0.5,
    1.0,
    2.5,
    5.0,
    10.0,
    30.0,
    f64::INFINITY,
];
#[cfg(feature = "metrics")]
const SP_INTERVAL_BUCKETS: [f64; 11] = [
    0f64,
    4f64,
    8f64,
    12f64,
    16f64,
    20f64,
    25f64,
    30f64,
    45f64,
    60f64,
    f64::INFINITY,
];

#[derive(Clone, Default)]
pub struct PlotCounts {
    pub og_plot_count: Arc<std::sync::atomic::AtomicU64>,
    pub nft_plot_count: Arc<std::sync::atomic::AtomicU64>,
    pub compresses_plot_count: Arc<std::sync::atomic::AtomicU64>,
    pub invalid_plot_count: Arc<std::sync::atomic::AtomicU64>,
    pub total_plot_space: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Clone)]
pub struct FarmerSharedState<T> {
    pub signage_points: Arc<RwLock<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub quality_to_identifiers: Arc<RwLock<HashMap<Bytes32, FarmerIdentifier>>>,
    pub proofs_of_space: ProofsMap,
    pub cache_time: Arc<RwLock<HashMap<Bytes32, Instant>>>,
    pub pool_states: Arc<RwLock<HashMap<Bytes32, FarmerPoolState>>>,
    pub farmer_private_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub owner_secret_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub owner_public_keys_to_auth_secret_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub pool_public_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub harvester_peers: PeerMap,
    pub most_recent_sp: Arc<RwLock<MostRecentSignagePoint>>,
    pub recent_errors: Arc<RwLock<RecentErrors<String>>>,
    pub running_state: Arc<RwLock<FarmerRunningState>>,
    pub missing_keys: Arc<RwLock<HashSet<Bytes48>>>,
    pub missing_plotnft_info: Arc<RwLock<HashMap<Bytes32, Bytes48>>>,
    pub upstream_handshake: Arc<RwLock<Option<Handshake>>>,
    pub plot_counts: Arc<PlotCounts>,
    pub data: Arc<T>,
    pub signal: Arc<AtomicBool>,
    pub additional_headers: Arc<HashMap<String, String>>,
    pub force_pool_update: Arc<AtomicBool>,
    pub last_pool_update: Arc<std::sync::atomic::AtomicU64>,
    pub last_sp_timestamp: Arc<RwLock<Instant>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<RwLock<Option<FarmerMetrics>>>,
}
impl<T: Default> Default for FarmerSharedState<T> {
    fn default() -> Self {
        Self {
            signage_points: Arc::new(Default::default()),
            quality_to_identifiers: Arc::new(Default::default()),
            proofs_of_space: Arc::new(Default::default()),
            cache_time: Arc::new(Default::default()),
            pool_states: Arc::new(Default::default()),
            farmer_private_keys: Arc::new(Default::default()),
            owner_secret_keys: Arc::new(Default::default()),
            owner_public_keys_to_auth_secret_keys: Arc::new(Default::default()),
            pool_public_keys: Arc::new(Default::default()),
            harvester_peers: Arc::new(Default::default()),
            most_recent_sp: Arc::new(Default::default()),
            recent_errors: Arc::new(Default::default()),
            running_state: Arc::new(Default::default()),
            missing_keys: Arc::new(Default::default()),
            missing_plotnft_info: Arc::new(Default::default()),
            upstream_handshake: Arc::new(Default::default()),
            plot_counts: Arc::new(Default::default()),
            data: Arc::new(T::default()),
            signal: Arc::new(Default::default()),
            additional_headers: Arc::new(Default::default()),
            force_pool_update: Arc::new(Default::default()),
            last_pool_update: Arc::new(Default::default()),
            last_sp_timestamp: Arc::new(RwLock::new(Instant::now())),
            #[cfg(feature = "metrics")]
            metrics: Arc::new(Default::default()),
        }
    }
}

#[cfg(feature = "metrics")]
#[derive(Debug, Clone)]
pub struct FarmerMetrics {
    pub id: Uuid,
    pub registry: Arc<RwLock<Registry>>,
    pub start_time: Arc<Instant>,
    pub uptime: Arc<GenericGauge<AtomicU64>>,
    pub last_signage_point_index: Arc<GenericGauge<AtomicU64>>,
    pub current_difficulty: Arc<GenericGaugeVec<AtomicU64>>,
    pub qualities_latency: Arc<Histogram>,
    pub proof_latency: Arc<Histogram>,
    pub signage_point_interval: Arc<Histogram>,
    pub signage_point_processing_latency: Arc<Histogram>,
    pub plot_load_latency: Arc<Histogram>,
    pub blockchain_synced: Arc<GenericGauge<AtomicU64>>,
    pub blockchain_height: Arc<GenericGauge<AtomicU64>>,
    pub blockchain_netspace: Arc<GenericGauge<AtomicU64>>,
    pub total_proofs_found: Arc<GenericCounter<AtomicU64>>,
    pub last_proofs_found: Arc<GenericGauge<AtomicU64>>,
    pub proofs_declared: Arc<GenericGauge<AtomicU64>>,
    pub total_partials_found: Arc<GenericCounter<AtomicU64>>,
    pub last_partials_found: Arc<GenericGauge<AtomicU64>>,
    pub total_passed_filter: Arc<GenericCounter<AtomicU64>>,
    pub last_passed_filter: Arc<GenericGauge<AtomicU64>>,
    pub partials_submitted: Arc<GenericCounterVec<AtomicU64>>,
    pub plot_file_size: Arc<GenericGaugeVec<AtomicU64>>,
    pub plot_counts: Arc<GenericGaugeVec<AtomicU64>>,
    pub points_acknowledged_24h: Arc<GenericGaugeVec<AtomicU64>>,
    pub points_found_24h: Arc<GenericGaugeVec<AtomicU64>>,
}
#[cfg(feature = "metrics")]
impl FarmerMetrics {
    pub fn new(registry: Registry, id: Uuid) -> Self {
        Self {
            id,
            start_time: Arc::new(Instant::now()),
            uptime: Arc::new(
                GenericGauge::new("uptime", "Is Upstream Node Synced")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            current_difficulty: Arc::new(
                GenericGaugeVec::new(
                    Opts::new("current_difficulty", "Current Difficulty"),
                    &["launcher_id"],
                )
                .inspect(|g: &GenericGaugeVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            proofs_declared: Arc::new(
                GenericGauge::new("proofs_declared", "Proofs of Space declared by this farmer")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            last_signage_point_index: Arc::new(
                GenericGauge::new("last_signage_point_index", "Index of Last Signage Point")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            points_acknowledged_24h: Arc::new(
                GenericGaugeVec::new(
                    Opts::new(
                        "points_acknowledged",
                        "Total points acknowledged by pool for all plot nfts in last 24 Hours",
                    ),
                    &["launcher_id"],
                )
                .inspect(|g: &GenericGaugeVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            points_found_24h: Arc::new(
                GenericGaugeVec::new(
                    Opts::new(
                        "points_found",
                        "Total points found for all plot nfts in last 24 Hours",
                    ),
                    &["launcher_id"],
                )
                .inspect(|g: &GenericGaugeVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            blockchain_synced: Arc::new(
                GenericGauge::new("blockchain_synced", "Is Upstream Node Synced")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            blockchain_height: Arc::new(
                GenericGauge::new("blockchain_height", "Blockchain Height")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            blockchain_netspace: Arc::new(
                GenericGauge::new("blockchain_netspace", "Current Netspace")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            qualities_latency: Arc::new({
                let opts = HistogramOpts::new(
                    "qualities_latency",
                    "Time in seconds to load plot data from Disk",
                )
                .buckets(LATENCY_BUCKETS.to_vec());
                Histogram::with_opts(opts)
                    .inspect(|h: &Histogram| {
                        registry.register(Box::new(h.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics")
            }),
            proof_latency: Arc::new({
                let opts = HistogramOpts::new(
                    "proof_latency",
                    "Time in seconds to compute a proof/partial",
                )
                .buckets(LATENCY_BUCKETS.to_vec());
                Histogram::with_opts(opts)
                    .inspect(|h: &Histogram| {
                        registry.register(Box::new(h.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics")
            }),
            signage_point_interval: Arc::new({
                let opts = HistogramOpts::new(
                    "signage_point_interval",
                    "Time in seconds in between signage points",
                )
                .buckets(SP_INTERVAL_BUCKETS.to_vec());
                Histogram::with_opts(opts)
                    .inspect(|h: &Histogram| {
                        registry.register(Box::new(h.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics")
            }),
            signage_point_processing_latency: Arc::new({
                let opts = HistogramOpts::new(
                    "signage_point_processing_latency",
                    "Time in seconds to process signage points",
                )
                .buckets(LATENCY_BUCKETS.to_vec());
                Histogram::with_opts(opts)
                    .inspect(|h: &Histogram| {
                        registry.register(Box::new(h.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics")
            }),
            plot_load_latency: Arc::new({
                let opts = HistogramOpts::new(
                    "plot_load_latency",
                    "Time in seconds to compute a plot file",
                )
                .buckets(PLOT_LOAD_LATENCY_BUCKETS.to_vec());
                Histogram::with_opts(opts)
                    .inspect(|h: &Histogram| {
                        registry.register(Box::new(h.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics")
            }),
            partials_submitted: Arc::new(
                GenericCounterVec::new(
                    Opts::new("partials_submitted", "Total Partials Submitted"),
                    &["launcher_id"],
                )
                .inspect(|g: &GenericCounterVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            total_proofs_found: Arc::new(
                GenericCounter::new("total_proofs_found", "Total Proofs Found")
                    .inspect(|g: &GenericCounter<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            last_proofs_found: Arc::new(
                GenericGauge::new("last_proofs_found", "Last Value of Proofs Found")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            total_partials_found: Arc::new(
                GenericCounter::new("total_partials_found", "Total Partials Found")
                    .inspect(|g: &GenericCounter<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            last_partials_found: Arc::new(
                GenericGauge::new("last_partials_found", "Last Value of Partials Found")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            total_passed_filter: Arc::new(
                GenericCounter::new("total_passed_filter", "Total Plots Passed Filter")
                    .inspect(|g: &GenericCounter<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            last_passed_filter: Arc::new(
                GenericGauge::new("last_passed_filter", "Last Value of Plots Passed Filter")
                    .inspect(|g: &GenericGauge<AtomicU64>| {
                        registry.register(Box::new(g.clone())).unwrap_or(());
                    })
                    .expect("Expected To Create Static Metrics"),
            ),
            plot_file_size: Arc::new(
                GenericGaugeVec::new(
                    Opts::new("plot_file_size", "Plots Loaded on Server"),
                    &["c_level", "k_size", "type"],
                )
                .inspect(|g: &GenericGaugeVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            plot_counts: Arc::new(
                GenericGaugeVec::new(
                    Opts::new("plot_counts", "Plots Loaded on Server"),
                    &["c_level", "k_size", "type"],
                )
                .inspect(|g: &GenericGaugeVec<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                })
                .expect("Expected To Create Static Metrics"),
            ),
            registry: Arc::new(RwLock::new(registry)),
        }
    }
}
