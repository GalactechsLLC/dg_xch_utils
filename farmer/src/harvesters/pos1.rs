pub use super::unified::UnifiedHarvester as Pos1Harvester;

use dg_xch_core::plots::{PlotFile, PlotHeader};
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::PlotReader;
use std::io::{Error, ErrorKind};
use tokio::fs::File;

pub fn supports(header: &PlotHeader) -> bool {
    matches!(header, PlotHeader::V1(header) if header.format_desc == b"v1.0")
}

pub async fn open(plot: DiskPlot<File>) -> Result<PlotReader<File, DiskPlot<File>>, Error> {
    if !supports(plot.header()) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "expected a standard PoS1 plot",
        ));
    }
    PlotReader::new(plot, None, None).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_core::plots::{PlotHeaderGHv2_5, PlotHeaderV1, PlotHeaderV2};

    #[test]
    fn only_standard_pos1_headers_are_supported() {
        let mut header = PlotHeaderV1 {
            format_desc: b"v1.0".to_vec(),
            ..Default::default()
        };
        assert!(supports(&PlotHeader::V1(header.clone())));
        header.format_desc = b"mmx-v3.0".to_vec();
        assert!(!supports(&PlotHeader::V1(header)));
        assert!(!supports(&PlotHeader::V2(PlotHeaderV2::default())));
        assert!(!supports(&PlotHeader::GHv2_5(PlotHeaderGHv2_5::default())));
    }
}
