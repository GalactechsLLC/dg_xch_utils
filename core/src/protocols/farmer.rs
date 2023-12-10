use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use crate::blockchain::pool_target::PoolTarget;
use crate::blockchain::proof_of_space::ProofOfSpace;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::config::PoolWalletConfig;
use dg_xch_macros::ChiaSerial;
use hyper::body::Buf;
use log::debug;
use serde::{Deserialize, Serialize};
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