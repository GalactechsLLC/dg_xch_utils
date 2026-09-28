use crate::{gpu_error, radix_kernels::kernels};
use cuda_core::simt::LaunchConfig;
use cuda_core::{CudaContext, CudaStream, DeviceBuffer};
use dg_xch_pos2::compute::check_cancelled;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const ITEMS: usize = 2048;
const BINS: usize = 256;
const PREFIX_ITEMS: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

fn dimensions(max_entries: usize) -> Result<(usize, usize), Error> {
    if max_entries > u32::MAX as usize {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "CUDA radix capacity exceeds the 32-bit entry limit",
        ));
    }
    let blocks = max_entries.div_ceil(ITEMS);
    Ok((blocks, blocks.div_ceil(PREFIX_ITEMS)))
}

pub fn scratch_bytes(max_entries: usize) -> Result<u64, Error> {
    let (blocks, chunks) = dimensions(max_entries)?;
    blocks
        .checked_add(chunks)
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_mul(BINS))
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_mul(size_of::<u32>()))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or_else(|| Error::other("CUDA radix metadata size overflow"))
}

pub struct Sorter {
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    module: kernels::LoadedModule,
    histogram: DeviceBuffer<u32>,
    sums: DeviceBuffer<u32>,
    bins: DeviceBuffer<u32>,
    errors: DeviceBuffer<u32>,
    max_entries: usize,
    coalesced: bool,
}

impl Sorter {
    pub fn new(
        context: Arc<CudaContext>,
        stream: Arc<CudaStream>,
        max_entries: usize,
    ) -> Result<Self, Error> {
        if !Arc::ptr_eq(&context, stream.context()) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "CUDA radix stream belongs to a different context",
            ));
        }
        scratch_bytes(max_entries)?;
        let (blocks, chunks) = dimensions(max_entries)?;
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let module = kernels::load(&context).map_err(gpu_error)?;
        let histogram = DeviceBuffer::zeroed(&stream, blocks * BINS).map_err(gpu_error)?;
        let sums = DeviceBuffer::zeroed(&stream, chunks * BINS).map_err(gpu_error)?;
        let bins = DeviceBuffer::zeroed(&stream, BINS).map_err(gpu_error)?;
        let errors = DeviceBuffer::zeroed(&stream, 1).map_err(gpu_error)?;
        Ok(Self {
            context,
            stream,
            module,
            histogram,
            sums,
            bins,
            errors,
            max_entries,
            coalesced: std::env::var("DGX_CUDA_RADIX_COALESCED").map_or(true, |value| value != "0"),
        })
    }

    fn wait(&self, cancelled: &AtomicBool) -> Result<(), Error> {
        let started = Instant::now();
        loop {
            check_cancelled(cancelled)?;
            if self.stream.query().map_err(gpu_error)? {
                return self.context.check_err().map_err(gpu_error);
            }
            if started.elapsed() >= TIMEOUT {
                return Err(Error::new(ErrorKind::TimedOut, "CUDA radix sort timed out"));
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    pub fn sort(
        &mut self,
        input: &mut DeviceBuffer<[u32; 4]>,
        scratch: &mut DeviceBuffer<[u32; 4]>,
        count: usize,
        final_table: bool,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        if count > self.max_entries
            || count > input.len()
            || count > scratch.len()
            || !Arc::ptr_eq(input.context(), &self.context)
            || !Arc::ptr_eq(scratch.context(), &self.context)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "CUDA radix input exceeds capacity or belongs to a different context",
            ));
        }
        if count == 0 {
            return Ok(());
        }
        self.context.bind_to_thread().map_err(gpu_error)?;
        self.context.check_err().map_err(gpu_error)?;
        let result = (|| {
            self.errors
                .copy_from_host(&self.stream, &[0])
                .map_err(gpu_error)?;
            let (blocks, chunks) = dimensions(count)?;
            let table_config = LaunchConfig {
                grid_dim: (blocks as u32, 1, 1),
                block_dim: (256, 1, 1),
                shared_mem_bytes: 0,
            };
            let chunk_config = LaunchConfig {
                grid_dim: (chunks as u32, BINS as u32, 1),
                ..table_config
            };
            let totals_config = LaunchConfig {
                grid_dim: (BINS as u32, 1, 1),
                ..table_config
            };
            let bins_config = LaunchConfig {
                grid_dim: (1, 1, 1),
                ..table_config
            };
            let bits = if final_table { 56 } else { 28 };
            for shift in (0..bits).step_by(8) {
                check_cancelled(cancelled)?;
                unsafe {
                    self.module.radix_histogram(
                        &self.stream,
                        table_config,
                        input.cu_deviceptr() as *const [u32; 4],
                        count as u32,
                        blocks as u32,
                        shift,
                        u32::from(final_table),
                        self.histogram.cu_deviceptr() as *mut u32,
                        self.errors.cu_deviceptr() as *mut u32,
                    )
                }
                .map_err(gpu_error)?;
                unsafe {
                    self.module.radix_prefix_chunks(
                        &self.stream,
                        chunk_config,
                        self.histogram.cu_deviceptr() as *mut u32,
                        blocks as u32,
                        chunks as u32,
                        self.sums.cu_deviceptr() as *mut u32,
                    )
                }
                .map_err(gpu_error)?;
                unsafe {
                    self.module.radix_prefix_totals(
                        &self.stream,
                        totals_config,
                        self.sums.cu_deviceptr() as *mut u32,
                        chunks as u32,
                        self.bins.cu_deviceptr() as *mut u32,
                    )
                }
                .map_err(gpu_error)?;
                unsafe {
                    self.module.radix_prefix_bins(
                        &self.stream,
                        bins_config,
                        self.bins.cu_deviceptr() as *mut u32,
                    )
                }
                .map_err(gpu_error)?;
                unsafe {
                    if self.coalesced {
                        self.module.radix_scatter_coalesced(
                            &self.stream,
                            table_config,
                            input.cu_deviceptr() as *const [u32; 4],
                            scratch.cu_deviceptr() as *mut [u32; 4],
                            count as u32,
                            blocks as u32,
                            chunks as u32,
                            shift,
                            u32::from(final_table),
                            self.histogram.cu_deviceptr() as *const u32,
                            self.sums.cu_deviceptr() as *const u32,
                            self.bins.cu_deviceptr() as *const u32,
                            self.errors.cu_deviceptr() as *mut u32,
                        )
                    } else {
                        self.module.radix_scatter(
                            &self.stream,
                            table_config,
                            input.cu_deviceptr() as *const [u32; 4],
                            scratch.cu_deviceptr() as *mut [u32; 4],
                            count as u32,
                            blocks as u32,
                            chunks as u32,
                            shift,
                            u32::from(final_table),
                            self.histogram.cu_deviceptr() as *const u32,
                            self.sums.cu_deviceptr() as *const u32,
                            self.bins.cu_deviceptr() as *const u32,
                            self.errors.cu_deviceptr() as *mut u32,
                        )
                    }
                }
                .map_err(gpu_error)?;
                std::mem::swap(input, scratch);
            }
            self.wait(cancelled)?;
            let mut errors = [0];
            self.errors
                .copy_to_host(&self.stream, &mut errors)
                .map_err(gpu_error)?;
            if errors[0] != 0 {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("CUDA radix rejected data: flags={:#x}", errors[0]),
                ));
            }
            check_cancelled(cancelled)
        })();
        if result.is_err() {
            self.stream.synchronize().map_err(gpu_error)?;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn radix_metadata_budget_is_checked_and_bounded() {
        assert_eq!(scratch_bytes(0).unwrap(), 1028);
        assert_eq!(scratch_bytes(1).unwrap(), 3076);
        assert_eq!(scratch_bytes(2048).unwrap(), scratch_bytes(1).unwrap());
        assert_eq!(scratch_bytes(2049).unwrap(), 4100);
        assert!(scratch_bytes(310_000_000).unwrap() < 160 * 1024 * 1024);
        assert!(scratch_bytes(usize::MAX).is_err());
    }

    #[test]
    #[ignore = "requires an explicitly selected CUDA GPU"]
    fn cuda_radix_matches_stable_cpu_sort_and_rejects_invalid_inputs() {
        let ordinal = std::env::var("DGX_CUDA_TEST_DEVICE")
            .expect("set DGX_CUDA_TEST_DEVICE to the selected hardware CUDA adapter")
            .parse::<usize>()
            .expect("DGX_CUDA_TEST_DEVICE must be an adapter ordinal");
        let context = CudaContext::new(ordinal).unwrap();
        let stream = context.default_stream();
        let cancelled = AtomicBool::new(false);
        let maximum = 2_097_169;
        let mut sorter = Sorter::new(context.clone(), stream.clone(), maximum).unwrap();
        for count in [0, 1, 31, 33, 257, 2047, 2048, 2049, 65_537, maximum] {
            for final_table in [false, true] {
                let values: Vec<[u32; 4]> = (0..count)
                    .map(|index| {
                        let state = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                        let key = if index % 17 == 0 {
                            0
                        } else if index % 19 == 0 {
                            (1u64 << 56) - 1
                        } else {
                            (state ^ (state >> 31)) & ((1u64 << 56) - 1)
                        };
                        if final_table {
                            [key as u32, (key >> 32) as u32, index as u32, 0]
                        } else {
                            [index as u32, 0, (key as u32) & ((1 << 28) - 1), 0]
                        }
                    })
                    .collect();
                let mut expected = values.clone();
                if final_table {
                    expected.sort_by_key(|entry| u64::from(entry[0]) | (u64::from(entry[1]) << 32));
                } else {
                    expected.sort_by_key(|entry| entry[2]);
                }
                for coalesced in [false, true] {
                    sorter.coalesced = coalesced;
                    let mut input = DeviceBuffer::from_host(&stream, &values).unwrap();
                    let mut scratch = DeviceBuffer::zeroed(&stream, count).unwrap();
                    sorter
                        .sort(&mut input, &mut scratch, count, final_table, &cancelled)
                        .unwrap();
                    assert_eq!(
                        input.to_host_vec(&stream).unwrap(),
                        expected,
                        "count={count}, final={final_table}, coalesced={coalesced}"
                    );
                }
            }
        }
        let mut input = DeviceBuffer::from_host(&stream, &[[0u32; 4]; 2]).unwrap();
        let mut scratch = DeviceBuffer::zeroed(&stream, 2).unwrap();
        assert_eq!(
            sorter
                .sort(&mut input, &mut scratch, 3, false, &cancelled)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        let mut small = Sorter::new(context, stream.clone(), 1).unwrap();
        assert_eq!(
            small
                .sort(&mut input, &mut scratch, 2, false, &cancelled)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            sorter
                .sort(&mut input, &mut scratch, 2, false, &cancelled)
                .unwrap_err()
                .kind(),
            ErrorKind::Interrupted
        );
        cancelled.store(false, Ordering::Relaxed);
        for (invalid, final_table) in [([0, 0, 1 << 28, 0], false), ([0, 1 << 24, 0, 0], true)] {
            input.copy_from_host(&stream, &[invalid; 2]).unwrap();
            assert_eq!(
                sorter
                    .sort(&mut input, &mut scratch, 2, final_table, &cancelled)
                    .unwrap_err()
                    .kind(),
                ErrorKind::InvalidData
            );
        }
    }
}
