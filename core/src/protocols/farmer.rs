use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use crate::config::PoolWalletConfig;
use crate::protocols::error::RecentErrors;
use crate::protocols::PeerMap;
use blst::min_pk::SecretKey;
use dg_xch_macros::ChiaSerial;
use hyper::body::Buf;
use log::debug;
#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericCounter, GenericGauge};
#[cfg(feature = "metrics")]
use prometheus::Registry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePoint {
    pub challenge_hash: Bytes32,
    pub challenge_chain_sp: Bytes32,
    pub reward_chain_sp: Bytes32,
    pub difficulty: u64,
    pub sub_slot_iters: u64,
    pub signage_point_index: u8,
    pub peak_height: u32,
}

impl dg_xch_serialize::ChiaSerialize for NewSignagePoint {
    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_chain_sp,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.reward_chain_sp,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(&self.difficulty));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.sub_slot_iters,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.signage_point_index,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(&self.peak_height));
        bytes
    }
    fn from_bytes<T: AsRef<[u8]>>(bytes: &mut std::io::Cursor<T>) -> Result<Self, std::io::Error>
    where
        Self: Sized,
    {
        let challenge_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let challenge_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let reward_chain_sp = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let difficulty = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let sub_slot_iters = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let signage_point_index = dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?;
        let peak_height = if bytes.remaining() >= 4 {
            //Maintain Compatibility with < Chia 2.X nodes for now
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes)?
        } else {
            debug!("You are connected to an old node version, Please update your Fullnode.");
            0u32
        };
        Ok(Self {
            challenge_hash,
            challenge_chain_sp,
            reward_chain_sp,
            difficulty,
            sub_slot_iters,
            signage_point_index,
            peak_height,
        })
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct DeclareProofOfSpace {
    pub challenge_hash: Bytes32,
    pub challenge_chain_sp: Bytes32,
    pub signage_point_index: u8,
    pub reward_chain_sp: Bytes32,
    pub proof_of_space: ProofOfSpace,
    pub challenge_chain_sp_signature: Bytes96,
    pub reward_chain_sp_signature: Bytes96,
    pub farmer_puzzle_hash: Bytes32,
    pub pool_target: Option<PoolTarget>,
    pub pool_signature: Option<Bytes96>,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignedValues {
    pub quality_string: Bytes32,
    pub foliage_block_data_hash: Bytes32,
    pub foliage_transaction_block_hash: Bytes32,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct FarmingInfo {
    pub challenge_hash: Bytes32,
    pub sp_hash: Bytes32,
    pub timestamp: u64,
    pub passed: u32,
    pub proofs: u32,
    pub total_plots: u32,
    pub lookup_time: u64,
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignedValues {
    pub quality_string: Bytes32,
    pub foliage_block_data_signature: Bytes96,
    pub foliage_transaction_block_signature: Bytes96,
}

pub type ProofsMap = Arc<Mutex<HashMap<Bytes32, Vec<(String, ProofOfSpace)>>>>;

#[derive(Debug)]
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

pub struct MostRecentSignagePoint {
    pub hash: Bytes32,
    pub index: u8,
    pub timestamp: Instant,
}
impl Default for MostRecentSignagePoint {
    fn default() -> Self {
        MostRecentSignagePoint {
            hash: Default::default(),
            index: 0,
            timestamp: Instant::now(),
        }
    }
}

#[derive(Default, Clone)]
pub struct FarmerSharedState {
    pub quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    pub signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub pool_state: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    pub cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    pub proofs_of_space: ProofsMap,
    pub farmer_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub farmer_private_keys: Arc<Mutex<Vec<SecretKey>>>,
    pub pool_public_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub owner_secret_keys: Arc<Mutex<HashMap<Bytes48, SecretKey>>>,
    pub harvester_peers: PeerMap,
    pub most_recent_sp: Arc<Mutex<MostRecentSignagePoint>>,
    pub recent_errors: Arc<Mutex<RecentErrors<String>>>,
    pub running_state: Arc<Mutex<FarmerRunningState>>,
    #[cfg(feature = "metrics")]
    pub metrics: Arc<Mutex<Option<FarmerMetrics>>>,
}

#[cfg(feature = "metrics")]
#[derive(Debug, Clone)]
pub struct FarmerMetrics {
    pub start_time: Arc<Instant>,
    pub uptime: Option<GenericGauge<AtomicU64>>,
    pub points_acknowledged_24h: Option<GenericGauge<AtomicU64>>,
    pub points_found_24h: Option<GenericGauge<AtomicU64>>,
    pub current_difficulty: Option<GenericGauge<AtomicU64>>,
    pub proofs_declared: Option<GenericCounter<AtomicU64>>,
    pub last_signage_point_index: Option<GenericGauge<AtomicU64>>,
}
#[cfg(feature = "metrics")]
impl FarmerMetrics {
    pub fn new(registry: &Registry) -> Self {
        let uptime = GenericGauge::new("farmer_uptime", "Uptime of Farmer").map_or(
            None,
            |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            },
        );
        let points_acknowledged_24h = GenericGauge::new(
            "points_acknowledged_24h",
            "Total points acknowledged by pool for all plot nfts",
        )
        .map_or(None, |g: GenericGauge<AtomicU64>| {
            registry.register(Box::new(g.clone())).unwrap_or(());
            Some(g)
        });
        let points_found_24h = GenericGauge::new(
            "points_found_24h",
            "Total points fount for all plot nfts",
        )
        .map_or(None, |g: GenericGauge<AtomicU64>| {
            registry.register(Box::new(g.clone())).unwrap_or(());
            Some(g)
        });
        let current_difficulty = GenericGauge::new("current_difficulty", "Current Difficulty")
            .map_or(None, |g: GenericGauge<AtomicU64>| {
                registry.register(Box::new(g.clone())).unwrap_or(());
                Some(g)
            });
        let proofs_declared =
            GenericCounter::new("proofs_declared", "Proofs of Space declared by this farmer")
                .map_or(None, |g: GenericCounter<AtomicU64>| {
                    registry.register(Box::new(g.clone())).unwrap_or(());
                    Some(g)
                });
        let last_signage_point_index = GenericGauge::new(
            "last_signage_point_index",
            "Index of Last Signage Point",
        )
        .map_or(None, |g: GenericGauge<AtomicU64>| {
            registry.register(Box::new(g.clone())).unwrap_or(());
            Some(g)
        });
        FarmerMetrics {
            start_time: Arc::new(Instant::now()),
            uptime,
            points_acknowledged_24h,
            points_found_24h,
            current_difficulty,
            proofs_declared,
            last_signage_point_index,
        }
    }
}
