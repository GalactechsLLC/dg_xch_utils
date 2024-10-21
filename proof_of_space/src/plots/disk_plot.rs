use crate::plots::plot_reader::read_plot_header_async;
use crate::utils::open_read_only_async;
use dg_xch_core::plots::{PlotFile, PlotHeader};
use std::fmt::{Display, Formatter};
use std::io::{Error, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt};
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct DiskPlot<F: AsyncSeek + AsyncRead> {
    file: Arc<Mutex<F>>,
    pub filename: Arc<PathBuf>,
    header: PlotHeader,
    plot_size: u64,
}
impl DiskPlot<tokio::fs::File> {
    pub async fn new(filename: &Path) -> Result<Self, Error> {
        let mut file = open_read_only_async(filename).await?;
        let plot_size = file.metadata().await?.len();
        let header = read_plot_header_async(&mut file).await?;
        file.seek(SeekFrom::Start(0)).await?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            filename: Arc::new(filename.to_path_buf()),
            header,
            plot_size,
        })
    }
}
impl<F: AsyncSeek + AsyncRead> Display for DiskPlot<F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            self.filename
                .file_name()
                .map_or("Invalid Path", |s| s.to_str().unwrap_or("Invalid Path")),
        )
    }
}
impl<'a, F: AsyncSeek + AsyncRead> PlotFile<'a, F> for DiskPlot<F> {
    fn header(&'a self) -> &'a PlotHeader {
        &self.header
    }

    fn plot_size(&'a self) -> &'a u64 {
        &self.plot_size
    }

    fn load_p7_park(&'a self, _index: u64) -> u128 {
        todo!()
    }

    fn file(&'a self) -> Arc<Mutex<F>> {
        self.file.clone()
    }
}
