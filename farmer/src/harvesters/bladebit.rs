use dg_xch_core::plots::{PlotFile, PlotHeader};
use dg_xch_pos::plots::decompressor::DecompressorPool;
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::PlotReader;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use tokio::fs::File;

pub struct BladebitHarvester;

impl BladebitHarvester {
    pub fn supports(header: &PlotHeader) -> bool {
        matches!(header, PlotHeader::V2(_))
    }

    pub async fn open(
        plot: DiskPlot<File>,
        pool: Arc<DecompressorPool>,
    ) -> Result<PlotReader<File, DiskPlot<File>>, Error> {
        if !Self::supports(plot.header()) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "expected a Bladebit plot",
            ));
        }
        PlotReader::new(plot, Some(pool.clone()), Some(pool)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::plots::{PlotHeaderGHv2_5, PlotHeaderV1, PlotHeaderV2};

    #[test]
    fn routes_only_bladebit_headers() {
        assert!(BladebitHarvester::supports(&PlotHeader::V2(
            PlotHeaderV2::default()
        )));
        assert!(!BladebitHarvester::supports(&PlotHeader::V1(
            PlotHeaderV1::default()
        )));
        assert!(!BladebitHarvester::supports(&PlotHeader::GHv2_5(
            PlotHeaderGHv2_5::default()
        )));
    }
}
