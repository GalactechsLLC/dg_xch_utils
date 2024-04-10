use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::reward_chain_block_unfinished::RewardChainBlockUnfinished;
use crate::blockchain::sized_bytes::{Bytes32, Bytes48, Bytes96};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
#[cfg(feature = "metrics")]
use std::sync::Arc;
#[cfg(feature = "metrics")]
use std::time::Instant;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PoolDifficulty {
    pub difficulty: u64,                    //Min Version 0.0.34
    pub sub_slot_iters: u64,                //Min Version 0.0.34
    pub pool_contract_puzzle_hash: Bytes32, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct HarvesterHandshake {
    pub farmer_public_keys: Vec<Bytes48>, //Min Version 0.0.34
    pub pool_public_keys: Vec<Bytes48>,   //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewSignagePointHarvester {
    pub challenge_hash: Bytes32,                //Min Version 0.0.34
    pub difficulty: u64,                        //Min Version 0.0.34
    pub sub_slot_iters: u64,                    //Min Version 0.0.34
    pub signage_point_index: u8,                //Min Version 0.0.34
    pub sp_hash: Bytes32,                       //Min Version 0.0.34
    pub pool_difficulties: Vec<PoolDifficulty>, //Min Version 0.0.34
    pub filter_prefix_bits: i8,                 //Min Version 0.0.35
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct ProofOfSpaceFeeInfo {
    pub applied_fee_threshold: u32, //Min Version 0.0.36
}

#[derive(ChiaSerial, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum SigningDataKind {
    FoliageBlockData = 1,
    FoliageTransactionBlock = 2,
    ChallengeChainVdf = 3,
    RewardChainVdf = 4,
    ChallengeChainSubSlot = 5,
    RewardChainSubSlot = 6,
    Partial = 7,
    Unknown = 255,
}
impl From<u8> for SigningDataKind {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::FoliageBlockData,
            2 => Self::FoliageTransactionBlock,
            3 => Self::ChallengeChainVdf,
            4 => Self::RewardChainVdf,
            5 => Self::ChallengeChainSubSlot,
            6 => Self::RewardChainSubSlot,
            7 => Self::Partial,
            _ => Self::Unknown,
        }
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SignatureRequestSourceData {
    pub kind: SigningDataKind, //Min Version 0.0.36
    pub data: Vec<u8>,         //Min Version 0.0.36
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct NewProofOfSpace {
    pub challenge_hash: Bytes32,                         //Min Version 0.0.34
    pub sp_hash: Bytes32,                                //Min Version 0.0.34
    pub plot_identifier: String,                         //Min Version 0.0.34
    pub proof: ProofOfSpace,                             //Min Version 0.0.34
    pub signage_point_index: u8,                         //Min Version 0.0.34
    pub include_source_signature_data: bool,             //Min Version 0.0.36
    pub farmer_reward_address_override: Option<Bytes32>, //Min Version 0.0.36
    pub fee_info: Option<ProofOfSpaceFeeInfo>,           //Min Version 0.0.36
}
impl dg_xch_serialize::ChiaSerialize for NewProofOfSpace {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.sp_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.plot_identifier,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.proof,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.signage_point_index,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.include_source_signature_data,
                version,
            ));
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.farmer_reward_address_override,
                version,
            ));
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.fee_info,
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
        let sp_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let plot_identifier = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let proof = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let signage_point_index = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let include_source_signature_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            false
        };
        let farmer_reward_address_override = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        let fee_info = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        Ok(Self {
            challenge_hash,
            sp_hash,
            plot_identifier,
            proof,
            signage_point_index,
            include_source_signature_data,
            farmer_reward_address_override,
            fee_info,
        })
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestSignatures {
    pub plot_identifier: String, //Min Version 0.0.34
    pub challenge_hash: Bytes32, //Min Version 0.0.34
    pub sp_hash: Bytes32,        //Min Version 0.0.34
    pub messages: Vec<Bytes32>,  //Min Version 0.0.34
    pub message_data: Option<Vec<Option<SignatureRequestSourceData>>>, //Min Version 0.0.36
    pub rc_block_unfinished: Option<RewardChainBlockUnfinished>, //Min Version 0.0.36
}
impl dg_xch_serialize::ChiaSerialize for RequestSignatures {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.plot_identifier,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.sp_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.messages,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.message_data,
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
        let plot_identifier = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let challenge_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let sp_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let messages = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let message_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
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
            plot_identifier,
            challenge_hash,
            sp_hash,
            messages,
            message_data,
            rc_block_unfinished,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondSignatures {
    pub plot_identifier: String,                         //Min Version 0.0.34
    pub challenge_hash: Bytes32,                         //Min Version 0.0.34
    pub sp_hash: Bytes32,                                //Min Version 0.0.34
    pub local_pk: Bytes48,                               //Min Version 0.0.34
    pub farmer_pk: Bytes48,                              //Min Version 0.0.34
    pub message_signatures: Vec<(Bytes32, Bytes96)>,     //Min Version 0.0.34
    pub include_source_signature_data: bool,             //Min Version 0.0.36
    pub farmer_reward_address_override: Option<Bytes32>, //Min Version 0.0.36
}

impl dg_xch_serialize::ChiaSerialize for RespondSignatures {
    fn to_bytes(&self, version: ChiaProtocolVersion) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.plot_identifier,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.challenge_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.sp_hash,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.local_pk,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.farmer_pk,
            version,
        ));
        bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
            &self.message_signatures,
            version,
        ));
        if version >= ChiaProtocolVersion::Chia0_0_36 {
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.include_source_signature_data,
                version,
            ));
            bytes.extend(dg_xch_serialize::ChiaSerialize::to_bytes(
                &self.farmer_reward_address_override,
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
        let plot_identifier = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let challenge_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let sp_hash = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let local_pk = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let farmer_pk = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let message_signatures = dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?;
        let include_source_signature_data = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            false
        };
        let farmer_reward_address_override = if version >= ChiaProtocolVersion::Chia0_0_36 {
            dg_xch_serialize::ChiaSerialize::from_bytes(bytes, version)?
        } else {
            None
        };
        Ok(Self {
            plot_identifier,
            challenge_hash,
            sp_hash,
            local_pk,
            farmer_pk,
            message_signatures,
            include_source_signature_data,
            farmer_reward_address_override,
        })
    }
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct Plot {
    pub filename: String,                           //Min Version 0.0.34
    pub size: u8,                                   //Min Version 0.0.34
    pub plot_id: Bytes32,                           //Min Version 0.0.34
    pub pool_public_key: Option<Bytes48>,           //Min Version 0.0.34
    pub pool_contract_puzzle_hash: Option<Bytes32>, //Min Version 0.0.34
    pub plot_public_key: Bytes48,                   //Min Version 0.0.34
    pub file_size: u64,                             //Min Version 0.0.34
    pub time_modified: u64,                         //Min Version 0.0.34
    pub compression_level: Option<u8>,              //Min Version 0.0.35
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RequestPlots {} //Min Version 0.0.34

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct RespondPlots {
    pub plots: Vec<Plot>,                      //Min Version 0.0.34
    pub failed_to_open_filenames: Vec<String>, //Min Version 0.0.34
    pub no_key_filenames: Vec<String>,         //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncIdentifier {
    pub timestamp: u64,  //Min Version 0.0.34
    pub sync_id: u64,    //Min Version 0.0.34
    pub message_id: u64, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncStart {
    pub identifier: PlotSyncIdentifier, //Min Version 0.0.34
    pub initial: bool,                  //Min Version 0.0.34
    pub last_sync_id: u64,              //Min Version 0.0.34
    pub plot_file_count: u32,           //Min Version 0.0.34
    pub harvesting_mode: u8,            //Min Version 0.0.35
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPathList {
    pub identifier: PlotSyncIdentifier, //Min Version 0.0.34
    pub data: Vec<String>,              //Min Version 0.0.34
    pub r#final: bool,                  //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncPlotList {
    pub identifier: PlotSyncIdentifier, //Min Version 0.0.34
    pub data: Vec<Plot>,                //Min Version 0.0.34
    pub r#final: bool,                  //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncDone {
    pub identifier: PlotSyncIdentifier, //Min Version 0.0.34
    pub duration: u64,                  //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncError {
    pub code: i16,                                       //Min Version 0.0.34
    pub message: String,                                 //Min Version 0.0.34
    pub expected_identifier: Option<PlotSyncIdentifier>, //Min Version 0.0.34
}

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct PlotSyncResponse {
    pub identifier: PlotSyncIdentifier, //Min Version 0.0.34
    pub message_type: i16,              //Min Version 0.0.34
    pub error: Option<PlotSyncError>,   //Min Version 0.0.34
}

use dg_xch_serialize::ChiaProtocolVersion;
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
