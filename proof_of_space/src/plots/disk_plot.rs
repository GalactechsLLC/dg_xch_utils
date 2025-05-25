use crate::constants::{K_MAX_BUCKETS, K_MEM_SORT_PROPORTION, K_MIN_BUCKETS};
use crate::entry_sizes::EntrySizes;
use crate::plots::plot_reader::read_plot_header_async;
use crate::utils::open_read_only_async;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::consensus::constants::ConsensusConstants;
use dg_xch_core::plots::{PlotFile, PlotHeader};
use log::info;
use std::cmp::min;
use std::fmt::{Display, Formatter};
use std::io::{Error, ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::OffsetDateTime;
use tokio::fs;
use tokio::io::{AsyncRead, AsyncSeek, AsyncSeekExt};
use tokio::sync::Mutex;
use tokio::time::Instant;

#[derive(Debug)]
pub struct DiskPlot<F: AsyncSeek + AsyncRead> {
    file: Arc<Mutex<F>>,
    pub filename: Arc<PathBuf>,
    header: PlotHeader,
    plot_size: u64,
}
impl DiskPlot<fs::File> {
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
    #[allow(clippy::too_many_arguments)]
    pub async fn create<P>(
        tmp1_dir: &Path,
        tmp2_dir: &Path,
        final_dir: &Path,
        _num_plots: usize,
        k: u8,
        _memo: &[u8],
        plot_id: Bytes32,
        constants: ConsensusConstants,
    ) -> Result<Self, Error> {
        let stripe_size = 65536;
        let num_threads = 8;
        let buf_megabytes = 4096;
        let filename = format!("dg_pos_{plot_id}_{}", OffsetDateTime::now_utc());
        let filename = Path::new(&filename);
        if k < constants.min_plot_size || k > constants.max_plot_size {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Plot size k= {} is invalid", k),
            ));
        }
        if buf_megabytes < 10 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Please provide at least 10MiB of RAM",
            ));
        }

        let thread_memory = (num_threads as u64)
            * (2 * (stripe_size + 5000)) as u64
            * (EntrySizes::get_max_entry_size(k, 4, true) as u64)
            / (1024 * 1024);

        let sub_mbytes = 5 + min((buf_megabytes as f64 * 0.05) as u64, 50) + thread_memory;

        if sub_mbytes > buf_megabytes as u64 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Please provide more memory. At least {} MiB", sub_mbytes),
            ));
        }

        let memory_size = ((buf_megabytes - sub_mbytes as u32) as u64) * 1024 * 1024;
        let mut max_table_size = 0.0;

        for i in 1..=7 {
            let memory_i = 1.3
                * ((1u64 << k) as f64)
                * EntrySizes::get_max_entry_size(k, i as u8, true) as f64;

            if memory_i > max_table_size {
                max_table_size = memory_i;
            }
        }

        let num_buckets = 2
            * (((max_table_size) / (memory_size as f64 * K_MEM_SORT_PROPORTION)).ceil() as u32)
                .next_power_of_two();

        if num_buckets < K_MIN_BUCKETS {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Minimum buckets is {}", K_MIN_BUCKETS),
            ));
        } else if num_buckets > K_MAX_BUCKETS {
            // if num_buckets_input != 0 {
            //     return Err(Box::new(InvalidValueException(format!(
            //         "Maximum buckets is {}",
            //         K_MAX_BUCKETS
            //     ))));
            // }
            let required_mem =
                (max_table_size / K_MAX_BUCKETS as f64) / K_MEM_SORT_PROPORTION / (1024.0 * 1024.0)
                    + sub_mbytes as f64;
            return Err(Error::new(
                ErrorKind::Unsupported,
                format!("Do not have enough memory. Need {:.2} MiB", required_mem),
            ));
        }

        let log_num_buckets = (num_buckets as f64).log2().ceil() as u32;

        assert!((num_buckets as f64).log2() == log_num_buckets as f64);

        if (max_table_size / num_buckets as f64) < stripe_size as f64 * 30.0f64 {
            return Err(Error::new(ErrorKind::InvalidInput, "Stripe size too large"));
        }

        info!(
            "\nStarting plotting progress into temporary dirs: {} and {}",
            tmp1_dir.display(),
            tmp2_dir.display()
        );
        info!("ID: {}", plot_id);
        info!("Plot size is: {}", k);
        info!("Buffer size is: {} MiB", buf_megabytes);
        info!("Using {} buckets", num_buckets);
        info!("Final Directory is: {}", final_dir.display());
        info!(
            "Using {} threads of stripe size {}",
            num_threads, stripe_size
        );
        info!("Process ID is: {}", std::process::id());
        let mut tmp_1_filenames: Vec<PathBuf> = Vec::new();
        tmp_1_filenames.push(Path::new(tmp1_dir).join(format!("{}.sort.tmp", filename.display())));
        for i in 1..=7 {
            tmp_1_filenames.push(Path::new(tmp1_dir).join(format!(
                "{}.table{}.tmp",
                filename.display(),
                i
            )));
        }
        let tmp_2_filename = Path::new(tmp2_dir).join(format!("{}.2.tmp", filename.display()));
        let _final_2_filename = Path::new(final_dir).join(format!("{}.2.tmp", filename.display()));
        let final_filename = Path::new(final_dir).join(filename);
        if !Path::new(tmp1_dir).exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Temp directory {} does not exist", tmp1_dir.display()),
            ));
        }

        if !Path::new(tmp2_dir).exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Temp2 directory {} does not exist", tmp2_dir.display()),
            ));
        }

        if !Path::new(final_dir).exists() {
            return Err(Error::new(
                ErrorKind::NotFound,
                format!("Final directory {} does not exist", final_dir.display()),
            ));
        }

        // Remove temporary files if they exist
        for p in &tmp_1_filenames {
            if p.exists() {
                fs::remove_file(p).await?;
            }
        }
        if tmp_2_filename.exists() {
            fs::remove_file(&tmp_2_filename).await?;
        }
        if final_filename.exists() {
            fs::remove_file(&final_filename).await?;
        }
        info!(
            "Starting phase 1/4: Forward Propagation into tmp files: {:?}",
            final_dir
        );
        let phase_1_start = Instant::now();
        // let table_size = phase1(
        //
        // ).await?;
        info!(
            "Phase 1 Completed in: {:.8} seconds",
            phase_1_start.elapsed().as_secs_f64()
        );
        todo!()
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
