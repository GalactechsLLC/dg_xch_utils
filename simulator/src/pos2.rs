use crate::plots::PlotKeys;
use chia_pos2::{Prover, QualityChain, create_v2_plot, solve_proof};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use std::io::Error;
use std::path::{Path, PathBuf};

/// Plots a simulated chain farms against.
///
/// A fixed set of small plots with deterministic ids, written once into a directory and reused on
/// later runs. The id and the filename both come from `(campaign_seed, plot_index)`, so a second
/// run of the same campaign finds its plots already on disk.
#[derive(Debug, Clone)]
pub struct PlotSet {
    pub dir: PathBuf,
    pub k: u8,
    pub strength: u8,
    pub testnet: bool,
    pub plots: Vec<Plot>,
}

#[derive(Debug, Clone)]
pub struct Plot {
    pub path: PathBuf,
    pub plot_id: Bytes32,
    pub keys: PlotKeys,
    /// False when the plot was already on disk and was reused.
    pub created: bool,
}

/// The 128 byte pool-public-key memo a plot carries: pool key, farmer key, then the master secret
/// the local key derives from.
fn memo(keys: &PlotKeys) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(&keys.pool_public_key.bytes());
    out.extend_from_slice(&keys.farmer.sk_to_pk().to_bytes());
    out.extend_from_slice(&keys.master.to_bytes());
    out
}

impl PlotSet {
    /// Create or reuse `count` plots under `dir`.
    pub fn setup(
        dir: &Path,
        campaign_seed: u64,
        count: u32,
        k: u8,
        strength: u8,
        testnet: bool,
    ) -> Result<Self, Error> {
        std::fs::create_dir_all(dir)?;
        let mut plots = Vec::with_capacity(count as usize);
        for index in 0..count {
            let keys = PlotKeys::derive(campaign_seed, index)?;
            let plot_index = u16::try_from(index).unwrap_or(u16::MAX);
            // The id a v2 proof will derive from its fields; created under anything else, the plot
            // would farm proofs no verifier accepts.
            let plot_id = keys.plot_id(strength, plot_index, 0);
            let path = dir.join(Self::file_name(k, strength, testnet, plot_id));
            let existed = path.exists();
            if !existed {
                create_v2_plot(
                    &path,
                    k,
                    strength,
                    &plot_id.bytes(),
                    plot_index,
                    0,
                    &memo(&keys),
                    testnet,
                )?;
            }
            plots.push(Plot {
                path,
                plot_id,
                keys,
                created: !existed,
            });
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            k,
            strength,
            testnet,
            plots,
        })
    }

    /// Deterministic, so a plot already on disk is recognised rather than remade.
    fn file_name(k: u8, strength: u8, testnet: bool, plot_id: Bytes32) -> String {
        let id: String = plot_id.bytes().iter().map(|b| format!("{b:02x}")).collect();
        let net = if testnet { "-testnet" } else { "" };
        format!("plot-k{k}-s{strength}{net}-{id}.plot")
    }

    /// Every quality chain this plot set holds for a challenge, tagged with the plot it came from.
    ///
    /// A challenge usually yields nothing: that is the plot filter doing its job, and it is why a
    /// simulated chain needs several plots to produce a proof at most signage points.
    pub fn qualities_for_challenge(
        &self,
        challenge: Bytes32,
    ) -> Result<Vec<(usize, QualityChain)>, Error> {
        let challenge = challenge.bytes();
        let mut found = Vec::new();
        for (index, plot) in self.plots.iter().enumerate() {
            let prover = Prover::new(&plot.path)?;
            for quality in prover.get_qualities_for_challenge(&challenge)? {
                found.push((index, quality));
            }
        }
        Ok(found)
    }

    /// Expand a quality chain into a full proof, in the packed form a block carries.
    #[must_use]
    pub fn solve(&self, plot_index: usize, quality: &QualityChain) -> Vec<u8> {
        let plot = &self.plots[plot_index];
        solve_proof(
            quality,
            &plot.plot_id.bytes(),
            self.k,
            self.strength,
            self.testnet,
        )
    }
}

#[cfg(test)]
#[path = "../tests/unit/pos2/tests.rs"]
mod tests;
