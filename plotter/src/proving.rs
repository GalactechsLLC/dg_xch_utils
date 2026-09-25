use crate::{PlotInfo, format, inspect};
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_pos2::chainer::{Chain, SearchLimits};
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::plotting::{NativePlot, PlotLimits};
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct ReconstructedPlot {
    pub info: PlotInfo,
    plot: NativePlot,
}

impl ReconstructedPlot {
    pub fn open(
        path: &Path,
        testnet: bool,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        Self::open_with_engine(path, testnet, limits, cancelled, NativePlot::build)
    }

    pub fn open_with_engine(
        path: &Path,
        testnet: bool,
        limits: PlotLimits,
        cancelled: &AtomicBool,
        engine: impl FnOnce(ProofParams, PlotLimits, &AtomicBool) -> Result<NativePlot, Error>,
    ) -> Result<Self, Error> {
        let info = inspect(path)?;
        if info.file_bytes > limits.memory_bytes {
            return Err(Error::other("plot verification file budget exceeded"));
        }
        let mut input = File::open(path)?;
        input.seek(SeekFrom::Start(43))?;
        let mut memo = zeroize::Zeroizing::new(vec![0u8; if info.portable { 112 } else { 128 }]);
        input.read_exact(&mut memo)?;
        let params = ProofParams::new(info.plot_id.into(), info.k, info.strength, testnet)?;
        let plot = engine(params.clone(), limits, cancelled)?;
        if plot.params() != &params {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "proving engine returned incorrect parameters",
            ));
        }
        let mut expected = tempfile::tempfile()?;
        format::write_plot(
            &mut expected,
            &plot,
            info.index,
            info.meta_group,
            &memo,
            cancelled,
        )?;
        if expected.metadata()?.len() != info.file_bytes
            || input.metadata()?.len() != info.file_bytes
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "plot differs from reconstructed canonical file",
            ));
        }
        input.rewind()?;
        expected.rewind()?;
        let mut actual_bytes = [0u8; 65_536];
        let mut expected_bytes = [0u8; 65_536];
        let mut remaining = info.file_bytes;
        while remaining > 0 {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::new(
                    ErrorKind::Interrupted,
                    "plot reconstruction cancelled",
                ));
            }
            let count = remaining.min(actual_bytes.len() as u64) as usize;
            input.read_exact(&mut actual_bytes[..count])?;
            expected.read_exact(&mut expected_bytes[..count])?;
            if actual_bytes[..count] != expected_bytes[..count] {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "plot content is corrupt, noncanonical or belongs to another PoS2 hash domain",
                ));
            }
            remaining -= count as u64;
        }
        Ok(Self { info, plot })
    }

    pub fn qualities(
        &self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<Chain>, Error> {
        self.plot.qualities(challenge, limits, cancelled)
    }

    pub fn prove(&self, chain: &Chain, challenge: Bytes32) -> Result<Vec<u8>, Error> {
        self.plot.prove(chain, challenge)
    }
}
