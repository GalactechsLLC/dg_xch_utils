mod network;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_plotter::proving::ReconstructedPlot;
use dg_xch_pos::pos2::chainer::SearchLimits;
use dg_xch_pos::pos2::plotting::PlotLimits;
use dg_xch_pos::pos2::quality::quality_hash;
use dg_xch_pos::pos2::{params::ProofParams, plotting::NativePlot};
pub use network::{Pos2Harvester, Pos2Status};
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

pub struct DiskHarvester {
    plot: dg_xch_plotter::reader::PlotReader,
    limits: PlotLimits,
}

impl DiskHarvester {
    pub fn open(
        path: &Path,
        testnet: bool,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        dg_xch_pos::pos2::compute::check_cancelled(cancelled)?;
        Ok(Self {
            plot: dg_xch_plotter::reader::PlotReader::open(path, testnet, limits.memory_bytes)?,
            limits,
        })
    }

    pub fn params(&self) -> &ProofParams {
        self.plot.params()
    }

    pub fn info(&self) -> &dg_xch_plotter::PlotInfo {
        &self.plot.info
    }

    pub fn qualities(
        &mut self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<dg_xch_pos::pos2::chainer::Chain>, Error> {
        self.plot.qualities(challenge, limits, cancelled)
    }

    pub fn prove_with_engine(
        &mut self,
        chain: &dg_xch_pos::pos2::chainer::Chain,
        challenge: Bytes32,
        cancelled: &AtomicBool,
        engine: &mut impl dg_xch_pos::pos2::compute::HashEngine,
    ) -> Result<CandidateProof, Error> {
        Ok(CandidateProof {
            quality: quality_hash(&chain.fragments, self.plot.info.strength),
            proof: self
                .plot
                .prove_with_engine(chain, challenge, self.limits, cancelled, engine)?,
        })
    }

    pub fn challenge_with_engine(
        &mut self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
        engine: &mut impl dg_xch_pos::pos2::compute::HashEngine,
    ) -> Result<Vec<CandidateProof>, Error> {
        self.qualities(challenge, limits, cancelled)?
            .iter()
            .map(|chain| self.prove_with_engine(chain, challenge, cancelled, engine))
            .collect()
    }

    pub fn challenge(
        &mut self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<CandidateProof>, Error> {
        let mut engine = dg_xch_pos::pos2::compute::CpuHasher::new(self.params());
        self.challenge_with_engine(challenge, limits, cancelled, &mut engine)
    }
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
