use crate::{PlotInfo, PlotRequest, create_with_writer, format};
use dg_xch_pos2::compact::CompactPlot;
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::plotting::PlotLimits;
use dg_xch_pos2::vulkan_full::{DevicePlot, build_device};
use std::io::{Error, Seek, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;

pub enum Plot {
    Device(DevicePlot),
    Host(CompactPlot),
}

impl Plot {
    pub fn build(
        params: ProofParams,
        ordinal: usize,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        if let Some(plot) = build_device(&params, ordinal, limits, cancelled)? {
            return Ok(Self::Device(plot));
        }
        let mut engine = dg_xch_pos2::vulkan::Hasher::for_params(&params, ordinal)?;
        CompactPlot::build_with_engine(params, limits, cancelled, &mut engine).map(Self::Host)
    }

    pub fn table_counts(&self) -> [usize; 4] {
        match self {
            Self::Device(plot) => plot.table_counts,
            Self::Host(plot) => plot.table_counts,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self,
        output: &mut (impl Write + Seek),
        index: u16,
        meta_group: u8,
        memo: &[u8],
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        match self {
            Self::Device(plot) => {
                let mut packed = plot.packed_chunks(memory_bytes, cancelled)?;
                format::write_packed_chunks(
                    output,
                    plot.params(),
                    packed.chunks(),
                    index,
                    meta_group,
                    memo,
                    cancelled,
                    |chunk| packed.chunk(chunk, cancelled),
                )
            }
            Self::Host(plot) => {
                format::write_compact(output, plot, index, meta_group, memo, cancelled)
            }
        }
    }
}

pub fn create(
    request: &PlotRequest,
    destination: &Path,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    ordinal: usize,
) -> Result<PlotInfo, Error> {
    create_with_writer(request, destination, cancelled, |params, output, memo| {
        Plot::build(params, ordinal, limits, cancelled)?.write(
            output,
            request.index,
            request.meta_group,
            memo,
            limits.memory_bytes,
            cancelled,
        )
    })
}
