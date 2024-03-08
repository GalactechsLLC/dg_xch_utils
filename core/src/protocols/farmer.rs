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

#[cfg(feature = "metrics")]
use prometheus::core::{AtomicU64, GenericCounter, GenericGauge};
#[cfg(feature = "metrics")]
use prometheus::Registry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

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
        }
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.foliage_transaction_block_data,
                version,
            ));
        }
        if version >= ChiaProtocolVersion::Chia0_0_36 {
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
pub struct FarmerSharedState<T> {
    pub signage_points: Arc<Mutex<HashMap<Bytes32, Vec<NewSignagePoint>>>>,
    pub quality_to_identifiers: Arc<Mutex<HashMap<Bytes32, FarmerIdentifier>>>,
    pub proofs_of_space: ProofsMap,
    pub cache_time: Arc<Mutex<HashMap<Bytes32, Instant>>>,
    pub pool_states: Arc<Mutex<HashMap<Bytes32, FarmerPoolState>>>,
    pub farmer_private_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub owner_secret_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub auth_secret_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub pool_public_keys: Arc<HashMap<Bytes48, SecretKey>>,
    pub harvester_peers: PeerMap,
    pub most_recent_sp: Arc<Mutex<MostRecentSignagePoint>>,
    pub recent_errors: Arc<Mutex<RecentErrors<String>>>,
    pub running_state: Arc<Mutex<FarmerRunningState>>,
    pub data: Arc<T>,
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
