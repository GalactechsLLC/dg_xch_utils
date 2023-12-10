extern crate core;

use async_trait::async_trait;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use crate::verifier::validate_proof;
use dg_xch_core::blockchain::proof_of_space::{
    calculate_pos_challenge, calculate_prefix_bits, passes_plot_filter, ProofOfSpace,
};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::consensus::constants::ConsensusConstants;
use log::warn;
use dg_xch_core::protocols::harvester::HarvesterState;
use crate::plots::disk_plot::DiskPlot;
use crate::plots::plot_reader::PlotReader;
use tokio::fs::File;
use tokio::sync::Mutex;

pub mod chacha8;
pub mod constants;
pub mod encoding;
pub mod entry_sizes;
pub mod f_calc;
pub mod finite_state_entropy;
pub mod plots;
pub mod utils;
pub mod verifier;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

pub fn verify_and_get_quality_string(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: &Bytes32,
    signage_point: &Bytes32,
    height: u32,
) -> Option<Bytes32> {
    if pos.pool_public_key.is_none() && pos.pool_contract_puzzle_hash.is_none() {
        warn!("Failed to Verify ProofOfSpace: null value for both pool_public_key and pool_contract_puzzle_hash");
        return None;
    }
    if pos.pool_public_key.is_some() && pos.pool_contract_puzzle_hash.is_some() {
        warn!("Failed to Verify ProofOfSpace: Non Null value for both for pool_public_key and pool_contract_puzzle_hash");
        return None;
    }
    if pos.size < constants.min_plot_size {
        warn!("Failed to Verify ProofOfSpace: Plot failed MIN_PLOT_SIZE");
        return None;
    }
    if pos.size > constants.max_plot_size {
        warn!("Failed to Verify ProofOfSpace: Plot failed MAX_PLOT_SIZE");
        return None;
    }
    if let Some(plot_id) = pos.get_plot_id() {
        if pos.challenge
            != calculate_pos_challenge(&plot_id, original_challenge_hash, signage_point)
        {
            warn!("Failed to Verify ProofOfSpace: New challenge is not challenge");
            return None;
        }
        let prefix_bits = if height == 0 {
            //Backwords compat with 1.8
            calculate_prefix_bits(constants, height)
        } else {
            constants.number_zero_bits_plot_filter as i8
        };
        if !passes_plot_filter(
            prefix_bits,
            &plot_id,
            original_challenge_hash,
            signage_point,
        ) {
            warn!("Failed to Verify ProofOfSpace: Plot Failed to Pass Filter");
            return None;
        }
        get_quality_string(pos, &plot_id)
    } else {
        None
    }
}

pub fn get_quality_string(pos: &ProofOfSpace, plot_id: &Bytes32) -> Option<Bytes32> {
    match validate_proof(
        plot_id.to_sized_bytes(),
        pos.size,
        pos.proof.as_ref(),
        pos.challenge.as_ref(),
    ) {
        Ok(q) => Some(q),
        Err(e) => {
            warn!("Failed to Validate Proof: {:?}", e);
            None
        }
    }
}


#[derive(Debug, Clone)]
pub struct PathInfo {
    pub path: PathBuf,
    pub file_name: String,
}
impl Hash for PathInfo {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.file_name.hash(state)
    }
}
impl Eq for PathInfo {}
impl PartialEq for PathInfo {
    fn eq(&self, other: &Self) -> bool {
        self.file_name == other.file_name
    }
}

#[derive(Debug)]
pub struct PlotInfo {
    pub reader: PlotReader<File, DiskPlot<File>>,
    pub pool_public_key: Option<Bytes48>,
    pub pool_contract_puzzle_hash: Option<Bytes32>,
    pub plot_public_key: Bytes48,
    pub file_size: u64,
    pub time_modified: u64,
}

#[async_trait]
pub trait PlotManagerAsync {
    fn set_public_keys(
        &mut self,
        farmer_public_keys: Vec<Bytes48>,
        pool_public_keys: Vec<Bytes48>,
    );
    async fn load_plots(
        &mut self,
        harvester_state: Arc<Mutex<HarvesterState>>,
    ) -> Result<(), Error>;
    fn plots(&self) -> &HashMap<PathInfo, Arc<PlotInfo>>;
    fn plots_ready(&self) -> Arc<AtomicBool>;
}