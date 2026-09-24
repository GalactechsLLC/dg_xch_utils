use crate::plots::disk_plot::DiskPlot;
use crate::plots::plot_reader::PlotReader;
use crate::verifier::validate_proof;
use async_trait::async_trait;
use dg_xch_core::blockchain::proof_of_space::ProofOfSpace;
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
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

pub use dg_xch_pos_common::finite_state_entropy;
pub mod chacha8;
pub mod constants;
pub mod encoding;
pub mod entry_sizes;
pub mod f_calc;
pub mod gigahorse;
pub mod gigahorse_cpu;
#[cfg(any(feature = "cuda", feature = "vulkan"))]
pub mod gigahorse_gpu;
pub mod plots;
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
