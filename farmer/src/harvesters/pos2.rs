use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_plotter::proving::ReconstructedPlot;
use dg_xch_pos::pos2::chainer::SearchLimits;
use dg_xch_pos::pos2::plotting::PlotLimits;
use dg_xch_pos::pos2::quality::quality_hash;
use dg_xch_pos::pos2::{params::ProofParams, plotting::NativePlot};
use std::io::Error;
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub struct DevelopmentHarvester {
    plot: ReconstructedPlot,
}

pub struct CandidateProof {
    pub quality: Bytes32,
    pub proof: Vec<u8>,
}

impl DevelopmentHarvester {
    pub fn open(
        path: &Path,
        testnet: bool,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        Ok(Self {
            plot: ReconstructedPlot::open(path, testnet, limits, cancelled)?,
        })
    }

    pub fn from_reconstructed(plot: ReconstructedPlot) -> Self {
        Self { plot }
    }

    pub fn open_with_engine(
        path: &Path,
        testnet: bool,
        limits: PlotLimits,
        cancelled: &AtomicBool,
        engine: impl FnOnce(ProofParams, PlotLimits, &AtomicBool) -> Result<NativePlot, Error>,
    ) -> Result<Self, Error> {
        Ok(Self {
            plot: ReconstructedPlot::open_with_engine(path, testnet, limits, cancelled, engine)?,
        })
    }

    pub fn challenge(
        &self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<CandidateProof>, Error> {
        self.plot
            .qualities(challenge, limits, cancelled)?
            .into_iter()
            .map(|chain| {
                Ok(CandidateProof {
                    quality: quality_hash(&chain.fragments, self.plot.info.strength),
                    proof: self.plot.prove(&chain, challenge)?,
                })
            })
            .collect()
    }
}
