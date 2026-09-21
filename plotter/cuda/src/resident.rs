use crate::{gpu_error, resident_kernels::kernels};
use cuda_core::simt::{LaunchConfig, memory};
use cuda_core::{CudaContext, CudaStream, DeviceBuffer, IntoResult};
use dg_xch_pos2::compact::CompactPlot;
use dg_xch_pos2::compact::resident::{
    Backend, BatchStatus, Entry, INDEX_BYTES, OUTPUT_ENTRIES, TRANSFER_BYTES,
};
use dg_xch_pos2::compute::{SCRATCH_BYTES, check_cancelled};
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::plotting::PlotLimits;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

fn device_memory_fits(max_entries: usize, available_bytes: u64) -> Result<bool, Error> {
    let required = CompactPlot::memory_required(28, max_entries)?
        .checked_sub(SCRATCH_BYTES)
        .and_then(|bytes| bytes.checked_div(2))
        .and_then(|bytes| bytes.checked_add(INDEX_BYTES))
        .and_then(|bytes| bytes.checked_add(TRANSFER_BYTES))
        .and_then(|bytes| bytes.checked_add(size_of::<[u32; 4]>() as u64))
        .and_then(|bytes| bytes.checked_add(DEVICE_HEADROOM_BYTES))
        .ok_or_else(|| Error::other("CUDA resident device memory budget overflow"))?;
    Ok(available_bytes >= required)
}

struct Resident {
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    module: kernels::LoadedModule,
    input: Option<DeviceBuffer<[u32; 4]>>,
    output: DeviceBuffer<[u32; 4]>,
    index: DeviceBuffer<u32>,
    counters: DeviceBuffer<u32>,
    parameters: [u32; 32],
}

impl Resident {
    fn new(context: Arc<CudaContext>, params: &ProofParams) -> Result<Self, Error> {
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let stream = context.default_stream();
        let module = kernels::load(&context).map_err(gpu_error)?;
        let output = DeviceBuffer::zeroed(&stream, OUTPUT_ENTRIES).map_err(gpu_error)?;
        let index = DeviceBuffer::zeroed(&stream, (INDEX_BYTES / 4) as usize).map_err(gpu_error)?;
        let counters = DeviceBuffer::zeroed(&stream, 4).map_err(gpu_error)?;
        let configuration = dg_xch_pos2::compute::config(params);
        let mut parameters = [0; 32];
        for (destination, bytes) in parameters[..8]
            .iter_mut()
            .zip(configuration.plot_id.as_chunks::<4>().0)
        {
            *destination = u32::from_le_bytes(*bytes);
        }
        for (destination, keys) in parameters[8..24]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(dg_xch_pos2::device::fragment_round_keys(configuration))
        {
            *destination = keys;
        }
        parameters[25] = u32::from(params.is_testnet());
        parameters[29] = OUTPUT_ENTRIES as u32;
        Ok(Self {
            context,
            stream,
            module,
            input: None,
            output,
            index,
            counters,
            parameters,
        })
    }

    fn activate(&self, cancelled: &AtomicBool) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        self.context.bind_to_thread().map_err(gpu_error)?;
        self.context.check_err().map_err(gpu_error)
    }

    fn wait(&self, cancelled: &AtomicBool) -> Result<(), Error> {
        let started = Instant::now();
        let result = (|| {
            loop {
                check_cancelled(cancelled)?;
                if self.stream.query().map_err(gpu_error)? {
                    return self.context.check_err().map_err(gpu_error);
                }
                if started.elapsed() >= TIMEOUT {
                    return Err(Error::new(
                        ErrorKind::TimedOut,
                        "CUDA resident operation timed out",
                    ));
                }
                std::thread::sleep(Duration::from_micros(100));
            }
        })();
        if result.is_err() {
            self.stream.synchronize().map_err(gpu_error)?;
        }
        result
    }

    fn configure(
        &mut self,
        table: u32,
        start: usize,
        count: usize,
        pair_budget: u64,
    ) -> Result<(), Error> {
        self.parameters[24] = table;
        self.parameters[26] =
            u32::try_from(start).map_err(|_| Error::other("CUDA resident start index overflow"))?;
        self.parameters[27] =
            u32::try_from(count).map_err(|_| Error::other("CUDA resident count overflow"))?;
        self.parameters[30] = pair_budget.min(u64::from(u32::MAX)) as u32;
        self.counters
            .copy_from_host(&self.stream, &[0; 4])
            .map_err(gpu_error)
    }

    fn status(&self, cancelled: &AtomicBool) -> Result<[u32; 4], Error> {
        self.wait(cancelled)?;
        let mut status = [0; 4];
        self.counters
            .copy_to_host(&self.stream, &mut status)
            .map_err(gpu_error)?;
        if status[2] != 0 {
            return Err(Error::other(format!(
                "CUDA resident table rejected: flags={:#x}, outputs={}, pairs={}",
                status[2], status[0], status[1]
            )));
        }
        check_cancelled(cancelled)?;
        Ok(status)
    }

    fn input(&self) -> Result<&DeviceBuffer<[u32; 4]>, Error> {
        self.input
            .as_ref()
            .ok_or_else(|| Error::other("CUDA resident input is not installed"))
    }
}

impl Backend for Resident {
    fn generate(
        &mut self,
        start: usize,
        count: usize,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.activate(cancelled)?;
        if count > OUTPUT_ENTRIES || start.checked_add(count).is_none_or(|end| end > 1 << 28) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid CUDA resident generation window",
            ));
        }
        if count == 0 {
            return Ok(());
        }
        self.configure(0, start, count, 0)?;
        unsafe {
            self.module.resident_generate(
                &self.stream,
                LaunchConfig::for_num_elems(count as u32),
                self.parameters,
                self.output.cu_deviceptr() as *mut [u32; 4],
                self.counters.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        self.status(cancelled)?;
        Ok(())
    }

    fn upload_index(
        &mut self,
        table: u32,
        entries: &[Entry],
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.activate(cancelled)?;
        if !(1..=3).contains(&table) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid CUDA resident table",
            ));
        }
        let count = u32::try_from(entries.len())
            .map_err(|_| Error::other("CUDA resident input count overflow"))?;
        self.release_inputs()?;
        let input =
            DeviceBuffer::<[u32; 4]>::zeroed(&self.stream, entries.len()).map_err(gpu_error)?;
        self.wait(cancelled)?;
        for (window, entries) in entries.chunks(OUTPUT_ENTRIES).enumerate() {
            self.activate(cancelled)?;
            let words: &[[u32; 4]] = bytemuck::try_cast_slice(entries)
                .map_err(|_| Error::other("invalid CUDA resident entry layout"))?;
            let offset = (window as u64)
                .checked_mul(TRANSFER_BYTES)
                .ok_or_else(|| Error::other("CUDA resident upload offset overflow"))?;
            let bytes = std::mem::size_of_val(words);
            if offset
                .checked_add(bytes as u64)
                .is_none_or(|end| end > input.num_bytes() as u64)
            {
                return Err(Error::other("CUDA resident upload exceeds its allocation"));
            }
            let destination = input
                .cu_deviceptr()
                .checked_add(offset)
                .ok_or_else(|| Error::other("CUDA resident device address overflow"))?;
            unsafe { memory::memcpy_htod_sync(destination, words.as_ptr(), bytes) }
                .map_err(gpu_error)?;
        }
        self.input = Some(input);
        self.parameters[28] = count;
        self.configure(table, 0, entries.len(), 0)?;
        if entries.is_empty() {
            return Ok(());
        }
        unsafe {
            self.module.resident_build_index(
                &self.stream,
                LaunchConfig::for_num_elems(count),
                self.parameters,
                self.input()?,
                self.index.cu_deviceptr() as *mut u32,
                self.counters.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        self.status(cancelled)?;
        Ok(())
    }

    fn match_table(
        &mut self,
        table: u32,
        start: usize,
        count: usize,
        pair_budget: u64,
        cancelled: &AtomicBool,
    ) -> Result<BatchStatus, Error> {
        self.activate(cancelled)?;
        if !(1..=3).contains(&table)
            || start
                .checked_add(count)
                .is_none_or(|end| end > self.input.as_ref().map_or(0, DeviceBuffer::len))
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid CUDA resident matching window",
            ));
        }
        if count == 0 {
            return Ok(BatchStatus {
                output_count: 0,
                pair_evaluations: 0,
            });
        }
        self.configure(table, start, count, pair_budget)?;
        let threads =
            u32::try_from(count).map_err(|_| Error::other("CUDA resident launch size overflow"))?;
        unsafe {
            let launch =
                if std::env::var_os("DGX_CUDA_REPLICATED_AES").is_none_or(|value| value != "0") {
                    kernels::LoadedModule::resident_match_table_replicated
                } else {
                    kernels::LoadedModule::resident_match_table
                };
            launch(
                &self.module,
                &self.stream,
                LaunchConfig::for_num_elems(threads),
                self.parameters,
                self.input()?,
                &self.index,
                self.output.cu_deviceptr() as *mut [u32; 4],
                self.counters.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        let status = self.status(cancelled)?;
        if status[0] as usize > OUTPUT_ENTRIES || u64::from(status[1]) > pair_budget {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "CUDA resident counters exceed the dispatch budget",
            ));
        }
        Ok(BatchStatus {
            output_count: status[0] as usize,
            pair_evaluations: u64::from(status[1]),
        })
    }

    fn read_entries(
        &mut self,
        count: usize,
        destination: &mut Vec<Entry>,
        capacity: usize,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.activate(cancelled)?;
        if count > self.output.len() || count > capacity.saturating_sub(destination.len()) {
            return Err(Error::other(
                "CUDA resident output exceeds the entry budget",
            ));
        }
        if count == 0 {
            return Ok(());
        }
        let previous = destination.len();
        let length = previous
            .checked_add(count)
            .ok_or_else(|| Error::other("CUDA resident output size overflow"))?;
        destination
            .try_reserve_exact(count)
            .map_err(|_| Error::other("CUDA resident output allocation failed"))?;
        destination.resize(length, Entry::default());
        let words: &mut [[u32; 4]] = bytemuck::try_cast_slice_mut(&mut destination[previous..])
            .map_err(|_| Error::other("invalid CUDA resident output layout"))?;
        let result = unsafe {
            cuda_core::sys::cuMemcpyDtoH_v2(
                words.as_mut_ptr().cast(),
                self.output.cu_deviceptr(),
                std::mem::size_of_val(words),
            )
        }
        .result()
        .map_err(gpu_error);
        if result.is_err() {
            destination.truncate(previous);
        }
        result?;
        check_cancelled(cancelled)
    }

    fn release_inputs(&mut self) -> Result<(), Error> {
        self.context.bind_to_thread().map_err(gpu_error)?;
        self.stream.synchronize().map_err(gpu_error)?;
        self.input = None;
        self.parameters[28] = 0;
        self.context.check_err().map_err(gpu_error)
    }
}

pub(super) fn build(
    context: Arc<CudaContext>,
    params: &ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<Option<CompactPlot>, Error> {
    let result = dg_xch_pos2::compact::resident::build(params, limits, cancelled, || {
        check_cancelled(cancelled)?;
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let mut available_bytes = 0;
        let mut total_bytes = 0;
        unsafe { cuda_core::sys::cuMemGetInfo_v2(&mut available_bytes, &mut total_bytes) }
            .result()
            .map_err(gpu_error)?;
        check_cancelled(cancelled)?;
        if !device_memory_fits(limits.max_entries, available_bytes as u64)? {
            return Ok(None);
        }
        Resident::new(context.clone(), params).map(Some)
    });
    let cleanup = context.check_err().map_err(gpu_error);
    let plot = result?;
    cleanup?;
    Ok(plot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_pos2::{compute, device};

    #[test]
    fn k28_resident_device_memory_preflight_limits() {
        let initial_entries = 1usize << 28;
        assert!(device_memory_fits(initial_entries - 1, u64::MAX).is_err());
        for (max_entries, required) in [
            (initial_entries, 5_502_926_868u64),
            (310_000_000, 6_040_846_356),
            (usize::MAX, 6_040_846_356),
        ] {
            assert!(!device_memory_fits(max_entries, 4 << 30).unwrap());
            assert!(!device_memory_fits(max_entries, required - 1).unwrap());
            assert!(device_memory_fits(max_entries, required).unwrap());
            assert!(device_memory_fits(max_entries, 8 << 30).unwrap());
        }
    }

    fn record(entry: Entry) -> device::Record {
        device::Record {
            meta: entry.meta,
            info: entry.info,
            x_bits: entry.x_bits,
            valid: 1,
            ..device::Record::default()
        }
    }

    fn fixture(
        configuration: device::Config,
        table: u32,
    ) -> (Entry, Vec<Entry>, Vec<(u64, u32, u32)>) {
        let left = Entry {
            meta: if table == 1 {
                0x0123_4567
            } else {
                0x007a_bcde_f012_3456
            },
            info: ((table - 1) << 26) | 0x0001_2345,
            x_bits: if table == 3 { 0x0123_4567 } else { 0 },
        };
        let mut rights = Vec::with_capacity(4);
        let mut expected = Vec::with_capacity(4);
        for key in 0..4u32 {
            let wanted = device::target(configuration, table, record(left), key);
            let (right, result) = (0..4096u32)
                .find_map(|candidate| {
                    let right = Entry {
                        meta: if table == 1 {
                            u64::from(key * 4096 + candidate)
                        } else {
                            (u64::from(key + 1) << 48)
                                | 0x0000_0432_1000_0000
                                | u64::from(candidate)
                        },
                        info: wanted,
                        x_bits: if table == 3 {
                            (key + 1) * 0x0001_2345
                        } else {
                            0
                        },
                    };
                    let result = device::pair(configuration, table, record(left), record(right));
                    (result.valid != 0).then_some((right, result))
                })
                .expect("bounded fixture search must find a passing pair");
            rights.push(right);
            expected.push((
                if table == 3 {
                    result.fragment
                } else {
                    result.meta
                },
                result.info,
                result.x_bits,
            ));
        }
        (left, rights, expected)
    }

    fn install_fixture(gpu: &mut Resident, entries: &[Entry], left_info: u32) {
        gpu.context.bind_to_thread().unwrap();
        gpu.release_inputs().unwrap();
        let words: &[[u32; 4]] = bytemuck::cast_slice(entries);
        gpu.input = Some(DeviceBuffer::from_host(&gpu.stream, words).unwrap());
        gpu.parameters[28] = entries.len() as u32;
        for (position, entry) in entries.iter().enumerate() {
            if entry.info == left_info {
                continue;
            }
            let bounds = [position as u32, position as u32 + 1];
            let offset = u64::from(entry.info) * size_of::<u32>() as u64;
            assert!(offset + size_of_val(&bounds) as u64 <= gpu.index.num_bytes() as u64);
            let destination = gpu.index.cu_deviceptr().checked_add(offset).unwrap();
            unsafe { memory::memcpy_htod_sync(destination, bounds.as_ptr(), size_of_val(&bounds)) }
                .unwrap();
        }
    }

    fn check_index_build(gpu: &mut Resident, cancelled: &AtomicBool) {
        let info_count = 1u32 << 28;
        let last = info_count - 4096;
        let mut entries: Vec<_> = (0..info_count)
            .step_by(4096)
            .map(|info| Entry {
                meta: u64::from(info),
                info,
                x_bits: 0,
            })
            .collect();
        entries.push(entries[0]);
        entries.push(*entries.iter().find(|entry| entry.info == last).unwrap());
        entries.sort_unstable_by_key(|entry| entry.info);
        gpu.upload_index(1, &entries, cancelled).unwrap();
        for query in [
            0,
            1,
            4095,
            4096,
            4097,
            last,
            last + 1,
            info_count - 1,
            info_count,
        ] {
            let offset = u64::from(query) * size_of::<u32>() as u64;
            assert!(offset + size_of::<u32>() as u64 <= gpu.index.num_bytes() as u64);
            let source = gpu.index.cu_deviceptr().checked_add(offset).unwrap();
            let mut actual = 0u32;
            unsafe {
                cuda_core::sys::cuMemcpyDtoH_v2((&raw mut actual).cast(), source, size_of::<u32>())
            }
            .result()
            .unwrap();
            let expected = entries.partition_point(|entry| entry.info < query) as u32;
            assert_eq!(actual, expected, "index query {query}");
        }

        for infos in [vec![1, 0], vec![info_count], vec![0, 4097]] {
            let malformed: Vec<_> = infos
                .into_iter()
                .map(|info| Entry {
                    info,
                    ..Entry::default()
                })
                .collect();
            assert!(gpu.upload_index(1, &malformed, cancelled).is_err());
            let mut status = [0; 4];
            gpu.counters.copy_to_host(&gpu.stream, &mut status).unwrap();
            assert_eq!(status, [0, 0, 4, 0]);
        }
        gpu.upload_index(1, &entries, cancelled).unwrap();
        assert_eq!(gpu.status(cancelled).unwrap(), [0; 4]);
        gpu.release_inputs().unwrap();
    }

    #[test]
    #[cfg(target_endian = "little")]
    #[ignore = "requires an explicit DGX_CUDA_TEST_DEVICE and a hardware CUDA GPU"]
    fn k28_resident_cuda_generation_matching_and_limits_match_cpu() {
        let ordinal = std::env::var("DGX_CUDA_TEST_DEVICE")
            .expect("set DGX_CUDA_TEST_DEVICE to the selected hardware CUDA adapter")
            .parse::<usize>()
            .expect("DGX_CUDA_TEST_DEVICE must be an adapter ordinal");
        let context = CudaContext::new(ordinal).unwrap();
        let cancelled = AtomicBool::new(false);
        for testnet in [false, true] {
            let params = ProofParams::new([37; 32].into(), 28, 2, testnet).unwrap();
            let configuration = compute::config(&params);
            let mut gpu = Resident::new(context.clone(), &params).unwrap();
            let count = 257;
            let start = (1usize << 28) - count;
            gpu.generate(start, count, &cancelled).unwrap();
            let mut generated = Vec::with_capacity(count);
            gpu.read_entries(count, &mut generated, count, &cancelled)
                .unwrap();
            for (offset, actual) in generated.iter().enumerate() {
                let expected = device::generate(configuration, (start + offset) as u32);
                assert_eq!(
                    (actual.meta, actual.info, actual.x_bits),
                    (expected.meta, expected.info, expected.x_bits)
                );
            }
            if !testnet {
                check_index_build(&mut gpu, &cancelled);
            }

            for table in 1..=3u32 {
                let (left, rights, expected_pair) = fixture(configuration, table);
                for left_count in [1usize, 257] {
                    let mut entries = vec![left; left_count];
                    entries.extend_from_slice(&rights);
                    entries.sort_unstable_by_key(|entry| entry.info);
                    let start = entries
                        .iter()
                        .position(|entry| entry.info == left.info)
                        .unwrap();
                    assert!(
                        entries[start..start + left_count]
                            .iter()
                            .all(|entry| entry.info == left.info)
                    );
                    install_fixture(&mut gpu, &entries, left.info);
                    let expected_count = (left_count * 4) as u32;
                    for (pair_budget, output_capacity, error_flag) in [
                        (0, expected_count, 2),
                        (expected_count, 0, 1),
                        (expected_count - 1, expected_count, 2),
                        (expected_count, expected_count - 1, 1),
                        (expected_count, expected_count, 0),
                    ] {
                        gpu.parameters[29] = output_capacity;
                        let result = gpu.match_table(
                            table,
                            start,
                            left_count,
                            u64::from(pair_budget),
                            &cancelled,
                        );
                        let mut status = [0; 4];
                        gpu.counters.copy_to_host(&gpu.stream, &mut status).unwrap();
                        assert!(status[0] <= output_capacity);
                        assert!(status[1] <= pair_budget);
                        if error_flag != 0 {
                            assert!(result.is_err(), "table {table} must reject kernel errors");
                            assert_ne!(status[2] & error_flag, 0, "table {table}: {status:?}");
                            continue;
                        }
                        let result = result.unwrap();
                        assert_eq!(status, [expected_count, expected_count, 0, 0]);
                        assert_eq!(result.output_count, expected_count as usize);
                        assert_eq!(result.pair_evaluations, u64::from(expected_count));
                        let mut output = Vec::with_capacity(expected_count as usize);
                        gpu.read_entries(
                            result.output_count,
                            &mut output,
                            expected_count as usize,
                            &cancelled,
                        )
                        .unwrap();
                        let mut actual: Vec<_> = output
                            .iter()
                            .map(|entry| (entry.meta, entry.info, entry.x_bits))
                            .collect();
                        let mut expected = expected_pair.repeat(left_count);
                        actual.sort_unstable();
                        expected.sort_unstable();
                        assert_eq!(
                            actual, expected,
                            "table {table}, {left_count} left entries, testnet={testnet}"
                        );
                    }
                    gpu.release_inputs().unwrap();
                }
            }
        }
        context.check_err().unwrap();
    }
}
