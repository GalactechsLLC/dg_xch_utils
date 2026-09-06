use crate::plots::disk_plot::DiskPlot;
use crate::plots::plot_reader::PlotReader;
use crate::verifier::validate_proof;
use async_trait::async_trait;
use dg_xch_core::blockchain::proof_of_space::{
    ProofOfSpace, calculate_plot_filter_input, calculate_prefix_bits, calculate_prefix_bits_v2,
    passes_plot_filter_input,
};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::protocols::harvester::HarvesterState;
use dg_xch_core::traits::SizedBytes;
use log::warn;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::fs::File;
use tokio::sync::RwLock;

pub mod chacha8;
pub mod constants;
pub mod encoding;
pub mod entry_sizes;
pub mod f_calc;
pub mod finite_state_entropy;
pub mod plots;
pub mod pos2;
pub mod util;
pub mod utils;
pub mod verifier;

fn _version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
fn _pkg_name() -> &'static str {
    env!("CARGO_PKG_NAME")
}

#[must_use]
pub fn version() -> String {
    format!("{}: {}", _pkg_name(), _version())
}

#[test]
fn test_version() {
    println!("{}", version());
}

#[must_use]
pub fn verify_and_get_quality_string(
    pos: &ProofOfSpace,
    constants: &ConsensusConstants,
    original_challenge_hash: Bytes32,
    signage_point: Bytes32,
    height: u32,
) -> Option<Bytes32> {
    if pos.pool_public_key.is_none() && pos.pool_contract_puzzle_hash.is_none() {
        warn!(
            "Failed to Verify ProofOfSpace: null value for both pool_public_key and pool_contract_puzzle_hash"
        );
        return None;
    }
    if pos.pool_public_key.is_some() && pos.pool_contract_puzzle_hash.is_some() {
        warn!(
            "Failed to Verify ProofOfSpace: Non Null value for both for pool_public_key and pool_contract_puzzle_hash"
        );
        return None;
    }
    if pos.version == 0 {
        if pos.size < constants.min_plot_size {
            warn!("Failed to Verify ProofOfSpace: Plot failed MIN_PLOT_SIZE");
            return None;
        }
        if pos.size > constants.max_plot_size {
            warn!("Failed to Verify ProofOfSpace: Plot failed MAX_PLOT_SIZE");
            return None;
        }
    } else if pos.strength < constants.min_plot_strength
        || pos.strength > constants.max_plot_strength
    {
        warn!("Failed to Verify ProofOfSpace: strength outside the allowed range");
        return None;
    }
    if let Some(plot_id) = pos.get_plot_id() {
        let filter_input =
            calculate_plot_filter_input(plot_id, original_challenge_hash, signage_point);
        if pos.challenge != Bytes32::new(dg_xch_core::utils::hash_256(filter_input)) {
            warn!("Failed to Verify ProofOfSpace: New challenge is not challenge");
            return None;
        }
        // v1 and v2 plots run different filters on different schedules.
        let prefix_bits = if pos.version == 0 {
            calculate_prefix_bits(constants, height)
        } else {
            calculate_prefix_bits_v2(constants, height)
        };
        if !passes_plot_filter_input(prefix_bits, filter_input) {
            warn!("Failed to Verify ProofOfSpace: Plot Failed to Pass Filter");
            return None;
        }
        if pos.version == 0 {
            get_quality_string(pos, &plot_id)
        } else {
            get_quality_string_v2(pos, plot_id, constants)
        }
    } else {
        None
    }
}

/// Validate a v2 proof and return its quality string: the hash of the quality chain commitment the
/// proof is a witness to.
#[must_use]
pub fn get_quality_string_v2(
    pos: &ProofOfSpace,
    plot_id: Bytes32,
    constants: &ConsensusConstants,
) -> Option<Bytes32> {
    let params = match pos2::params::ProofParams::new(
        plot_id,
        constants.plot_size_v2,
        pos.strength,
        constants.is_testnet,
    ) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to Verify ProofOfSpace: {e}");
            return None;
        }
    };
    let validator = match pos2::validator::ProofValidator::new(params) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to Verify ProofOfSpace: {e}");
            return None;
        }
    };
    let xs = pos2::bits::expand_bits(pos.proof.as_ref(), constants.plot_size_v2)?;
    let xs: [u32; pos2::constants::TOTAL_XS_IN_PROOF] = match xs.try_into() {
        Ok(xs) => xs,
        Err(_) => {
            warn!("Failed to Verify ProofOfSpace: proof is not 128 x values");
            return None;
        }
    };
    let fragments = validator.validate_full_proof(&xs, pos.challenge)?;
    Some(pos2::quality::quality_hash(&fragments, pos.strength))
}

#[must_use]
pub fn get_quality_string(pos: &ProofOfSpace, plot_id: &Bytes32) -> Option<Bytes32> {
    match validate_proof(
        &plot_id.bytes(),
        pos.size,
        pos.proof.as_ref(),
        pos.challenge.as_ref(),
    ) {
        Ok(q) => Some(q),
        Err(e) => {
            warn!("Failed to Validate Proof: {e:?}");
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
        self.file_name.hash(state);
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
    fn set_public_keys(&mut self, farmer_public_keys: Vec<Bytes48>, pool_public_keys: Vec<Bytes48>);
    async fn load_plots(
        &mut self,
        harvester_state: Arc<RwLock<HarvesterState>>,
    ) -> Result<(), Error>;
    fn plots(&self) -> &HashMap<PathInfo, Arc<PlotInfo>>;
    fn plots_ready(&self) -> Arc<AtomicBool>;
}
