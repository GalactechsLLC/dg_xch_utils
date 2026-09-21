use crate::{gpu_error, radix, resident_kernels::kernels};
use cuda_core::simt::LaunchConfig;
use cuda_core::{CudaContext, CudaStream, DeviceBuffer, IntoResult};
use dg_xch_pos2::compact::resident::{INDEX_BYTES, OUTPUT_ENTRIES, TRANSFER_BYTES};
use dg_xch_pos2::compact::{CompactPlot, Entry};
use dg_xch_pos2::compute::{SCRATCH_BYTES, allocate, check_cancelled};
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::plotting::PlotLimits;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;
const INITIAL_ENTRIES: usize = 1 << 28;
const GENERATION_BATCH: usize = 1 << 24;
const MATCHING_BATCH: usize = 1 << 23;

struct MemoryPlan {
    capacity: usize,
    device_bytes: u64,
    managed_bytes: u64,
}

fn memory_plan(max_entries: usize) -> Result<MemoryPlan, Error> {
    let tables = CompactPlot::memory_required(28, max_entries)?
        .checked_sub(SCRATCH_BYTES)
        .ok_or_else(|| Error::other("CUDA full-resident table memory overflow"))?;
    let capacity = usize::try_from(tables / (2 * size_of::<Entry>() as u64))
        .map_err(|_| Error::other("CUDA full-resident capacity exceeds address space"))?;
    let sort_bytes = radix::scratch_bytes(capacity)?;
    let device_bytes = tables
        .checked_add(INDEX_BYTES)
        .and_then(|bytes| bytes.checked_add(size_of::<[u32; 4]>() as u64))
        .and_then(|bytes| bytes.checked_add(sort_bytes))
        .ok_or_else(|| Error::other("CUDA full-resident device memory overflow"))?;
    let managed_bytes = device_bytes
        .checked_add(SCRATCH_BYTES)
        .ok_or_else(|| Error::other("CUDA full-resident managed memory overflow"))?;
    Ok(MemoryPlan {
        capacity,
        device_bytes,
        managed_bytes,
    })
}

fn fits(plan: &MemoryPlan, limits: PlotLimits, available: u64) -> Result<bool, Error> {
    let required = plan
        .device_bytes
        .checked_add(DEVICE_HEADROOM_BYTES)
        .ok_or_else(|| Error::other("CUDA full-resident headroom overflow"))?;
    Ok(plan.managed_bytes <= limits.memory_bytes && required <= available)
}

fn wait(context: &CudaContext, stream: &CudaStream, cancelled: &AtomicBool) -> Result<(), Error> {
    let started = Instant::now();
    let result = (|| {
        loop {
            check_cancelled(cancelled)?;
            if stream.query().map_err(gpu_error)? {
                return context.check_err().map_err(gpu_error);
            }
            if started.elapsed() >= TIMEOUT {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "CUDA full-resident operation timed out",
                ));
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    })();
    if result.is_err() {
        stream.synchronize().map_err(gpu_error)?;
    }
    result
}

fn device_offset(buffer: &DeviceBuffer<[u32; 4]>, offset: usize) -> Result<u64, Error> {
    if offset > buffer.len() {
        return Err(Error::other(
            "CUDA full-resident device offset exceeds buffer",
        ));
    }
    let bytes = (offset as u64)
        .checked_mul(size_of::<[u32; 4]>() as u64)
        .ok_or_else(|| Error::other("CUDA full-resident byte offset overflow"))?;
    buffer
        .cu_deviceptr()
        .checked_add(bytes)
        .ok_or_else(|| Error::other("CUDA full-resident device address overflow"))
}

fn charge(remaining: &mut u64, amount: u64, cancelled: &AtomicBool) -> Result<(), Error> {
    check_cancelled(cancelled)?;
    *remaining = remaining
        .checked_sub(amount)
        .ok_or_else(|| Error::other("CUDA full-resident plotting work budget exceeded"))?;
    Ok(())
}

struct Resident {
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    module: kernels::LoadedModule,
    input: DeviceBuffer<[u32; 4]>,
    scratch: DeviceBuffer<[u32; 4]>,
    index: DeviceBuffer<u32>,
    counters: DeviceBuffer<u32>,
    sorter: radix::Sorter,
    parameters: [u32; 32],
}

impl Resident {
    fn new(
        context: Arc<CudaContext>,
        params: &ProofParams,
        capacity: usize,
    ) -> Result<Self, Error> {
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let stream = context.default_stream();
        let module = kernels::load(&context).map_err(gpu_error)?;
        let input = DeviceBuffer::zeroed(&stream, capacity).map_err(gpu_error)?;
        let scratch = DeviceBuffer::zeroed(&stream, capacity).map_err(gpu_error)?;
        let index = DeviceBuffer::zeroed(&stream, (INDEX_BYTES / 4) as usize).map_err(gpu_error)?;
        let counters = DeviceBuffer::zeroed(&stream, 4).map_err(gpu_error)?;
        let sorter = radix::Sorter::new(context.clone(), stream.clone(), capacity)?;
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
        Ok(Self {
            context,
            stream,
            module,
            input,
            scratch,
            index,
            counters,
            sorter,
            parameters,
        })
    }

    fn configure(
        &mut self,
        table: u32,
        range: std::ops::Range<usize>,
        input_count: usize,
        output_capacity: usize,
        pair_budget: u64,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        self.parameters[24] = table;
        self.parameters[26] = u32::try_from(range.start)
            .map_err(|_| Error::other("CUDA full-resident start index overflow"))?;
        self.parameters[27] = u32::try_from(range.len())
            .map_err(|_| Error::other("CUDA full-resident batch count overflow"))?;
        self.parameters[28] = u32::try_from(input_count)
            .map_err(|_| Error::other("CUDA full-resident input count overflow"))?;
        self.parameters[29] = u32::try_from(output_capacity)
            .map_err(|_| Error::other("CUDA full-resident output capacity overflow"))?;
        self.parameters[30] = pair_budget.min(u64::from(u32::MAX)) as u32;
        self.counters
            .copy_from_host(&self.stream, &[0; 4])
            .map_err(gpu_error)
    }

    fn status(&self, cancelled: &AtomicBool) -> Result<[u32; 4], Error> {
        wait(&self.context, &self.stream, cancelled)?;
        let mut status = [0; 4];
        self.counters
            .copy_to_host(&self.stream, &mut status)
            .map_err(gpu_error)?;
        if status[2] != 0 {
            return Err(Error::other(format!(
                "CUDA full-resident table rejected: flags={:#x}, outputs={}, pairs={}",
                status[2], status[0], status[1],
            )));
        }
        check_cancelled(cancelled)?;
        Ok(status)
    }
}

pub(super) struct DevicePlot {
    pub context: Arc<CudaContext>,
    pub stream: Arc<CudaStream>,
    pub entries: DeviceBuffer<[u32; 4]>,
    pub count: usize,
    pub params: ProofParams,
    pub table_counts: [usize; 4],
}

impl DevicePlot {
    pub fn download(self, cancelled: &AtomicBool) -> Result<CompactPlot, Error> {
        check_cancelled(cancelled)?;
        if self.count > self.entries.len() || self.count > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "CUDA fragment count exceeds its device allocation",
            ));
        }
        if self.count == 0 {
            return CompactPlot::from_sorted_fragments(
                self.params,
                Vec::new(),
                self.table_counts,
                cancelled,
            );
        }
        let Self {
            context,
            stream,
            entries,
            count,
            params,
            table_counts,
        } = self;
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let started = Instant::now();
        let module = kernels::load(&context).map_err(gpu_error)?;
        let fragments = DeviceBuffer::<u64>::zeroed(&stream, count).map_err(gpu_error)?;
        let counters = DeviceBuffer::<u32>::zeroed(&stream, 4).map_err(gpu_error)?;
        for start in (0..count).step_by(OUTPUT_ENTRIES) {
            check_cancelled(cancelled)?;
            let length = OUTPUT_ENTRIES.min(count - start);
            let destination = fragments
                .cu_deviceptr()
                .checked_add(start as u64 * size_of::<u64>() as u64)
                .ok_or_else(|| Error::other("CUDA fragment extraction offset overflow"))?;
            unsafe {
                module.resident_extract_fragments(
                    &stream,
                    LaunchConfig::for_num_elems(length as u32),
                    &entries,
                    start as u32,
                    length as u32,
                    destination as *mut u64,
                    counters.cu_deviceptr() as *mut u32,
                )
            }
            .map_err(gpu_error)?;
        }
        wait(&context, &stream, cancelled)?;
        let mut status = [0; 4];
        counters
            .copy_to_host(&stream, &mut status)
            .map_err(gpu_error)?;
        if status[2] != 0 {
            return Err(Error::other("CUDA final fragment extraction rejected"));
        }
        drop(entries);
        drop(counters);
        context.check_err().map_err(gpu_error)?;
        let mut output = allocate(count)?;
        let readback_entries = TRANSFER_BYTES as usize / size_of::<u64>();
        for start in (0..count).step_by(readback_entries) {
            check_cancelled(cancelled)?;
            let length = readback_entries.min(count - start);
            output.resize(start + length, 0u64);
            let source = fragments
                .cu_deviceptr()
                .checked_add(start as u64 * size_of::<u64>() as u64)
                .ok_or_else(|| Error::other("CUDA fragment readback offset overflow"))?;
            unsafe {
                cuda_core::sys::cuMemcpyDtoH_v2(
                    output[start..].as_mut_ptr().cast(),
                    source,
                    length * size_of::<u64>(),
                )
            }
            .result()
            .map_err(gpu_error)?;
        }
        drop(fragments);
        context.check_err().map_err(gpu_error)?;
        if std::env::var_os("DGX_POS2_PROFILE").is_some() {
            eprintln!(
                "pos2_cuda_resident download seconds={:.3} bytes={}",
                started.elapsed().as_secs_f64(),
                count as u64 * size_of::<u64>() as u64,
            );
        }
        CompactPlot::from_sorted_fragments(params, output, table_counts, cancelled)
    }
}

pub(super) fn build_device(
    context: Arc<CudaContext>,
    params: &ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<Option<DevicePlot>, Error> {
    check_cancelled(cancelled)?;
    if params.k() != 28 || params.strength() != 2 || !cfg!(target_endian = "little") {
        return Ok(None);
    }
    let plan = memory_plan(limits.max_entries)?;
    if limits.max_work < CompactPlot::minimum_work(params) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "insufficient CUDA full-resident plotting work budget",
        ));
    }
    context.bind_to_thread().map_err(gpu_error)?;
    context.check_err().map_err(gpu_error)?;
    let mut available = 0;
    let mut total = 0;
    unsafe { cuda_core::sys::cuMemGetInfo_v2(&mut available, &mut total) }
        .result()
        .map_err(gpu_error)?;
    if !fits(&plan, limits, available as u64)? {
        if std::env::var_os("DGX_POS2_PROFILE").is_some() {
            eprintln!(
                "pos2_cuda_resident unavailable managed_required={} managed_budget={} device_required={} device_available={available}",
                plan.managed_bytes,
                limits.memory_bytes,
                plan.device_bytes + DEVICE_HEADROOM_BYTES,
            );
        }
        return Ok(None);
    }
    check_cancelled(cancelled)?;
    let profile = std::env::var_os("DGX_POS2_PROFILE").is_some();
    if profile {
        eprintln!(
            "pos2_cuda_resident selected capacity={} managed_bytes={} device_bytes={}",
            plan.capacity, plan.managed_bytes, plan.device_bytes,
        );
    }
    let mut gpu = Resident::new(context.clone(), params, plan.capacity)?;
    let mut remaining_work = limits.max_work;
    let started = Instant::now();
    for start in (0..INITIAL_ENTRIES).step_by(GENERATION_BATCH) {
        let count = GENERATION_BATCH.min(INITIAL_ENTRIES - start);
        charge(&mut remaining_work, count as u64, cancelled)?;
        gpu.configure(
            0,
            start..start + count,
            0,
            plan.capacity - start,
            0,
            cancelled,
        )?;
        unsafe {
            gpu.module.resident_generate(
                &gpu.stream,
                LaunchConfig::for_num_elems(count as u32),
                gpu.parameters,
                device_offset(&gpu.input, start)? as *mut [u32; 4],
                gpu.counters.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        gpu.status(cancelled)?;
    }
    if profile {
        eprintln!(
            "pos2_cuda_resident generation seconds={:.3}",
            started.elapsed().as_secs_f64(),
        );
    }
    let mut count = INITIAL_ENTRIES;
    let mut table_counts = [count, 0, 0, 0];
    let replicated_aes =
        std::env::var_os("DGX_CUDA_REPLICATED_AES").is_none_or(|value| value != "0");
    for table in 1..=3u32 {
        let started = Instant::now();
        gpu.sorter
            .sort(&mut gpu.input, &mut gpu.scratch, count, false, cancelled)?;
        if profile {
            eprintln!(
                "pos2_cuda_resident sort_{} seconds={:.3}",
                table - 1,
                started.elapsed().as_secs_f64(),
            );
        }
        let started = Instant::now();
        if count != 0 {
            gpu.configure(table, 0..count, count, 0, 0, cancelled)?;
            unsafe {
                gpu.module.resident_build_index(
                    &gpu.stream,
                    LaunchConfig::for_num_elems(count as u32),
                    gpu.parameters,
                    &gpu.input,
                    gpu.index.cu_deviceptr() as *mut u32,
                    gpu.counters.cu_deviceptr() as *mut u32,
                )
            }
            .map_err(gpu_error)?;
            gpu.status(cancelled)?;
        }
        if profile {
            eprintln!(
                "pos2_cuda_resident index_{table} seconds={:.3}",
                started.elapsed().as_secs_f64(),
            );
        }
        let started = Instant::now();
        let mut output_count = 0usize;
        for start in (0..count).step_by(MATCHING_BATCH) {
            let length = MATCHING_BATCH.min(count - start);
            charge(&mut remaining_work, length as u64 * 4, cancelled)?;
            let capacity = plan.capacity - output_count;
            gpu.configure(
                table,
                start..start + length,
                count,
                capacity,
                remaining_work,
                cancelled,
            )?;
            unsafe {
                let launch = if replicated_aes {
                    kernels::LoadedModule::resident_match_table_replicated
                } else {
                    kernels::LoadedModule::resident_match_table
                };
                launch(
                    &gpu.module,
                    &gpu.stream,
                    LaunchConfig::for_num_elems(length as u32),
                    gpu.parameters,
                    &gpu.input,
                    &gpu.index,
                    device_offset(&gpu.scratch, output_count)? as *mut [u32; 4],
                    gpu.counters.cu_deviceptr() as *mut u32,
                )
            }
            .map_err(gpu_error)?;
            let status = gpu.status(cancelled)?;
            if status[0] as usize > capacity || u64::from(status[1]) > remaining_work {
                return Err(Error::other(
                    "CUDA full-resident counters exceed the dispatch budget",
                ));
            }
            charge(&mut remaining_work, u64::from(status[1]), cancelled)?;
            output_count += status[0] as usize;
        }
        count = output_count;
        table_counts[table as usize] = count;
        std::mem::swap(&mut gpu.input, &mut gpu.scratch);
        if profile {
            eprintln!(
                "pos2_cuda_resident matching_{table} seconds={:.3} entries={count}",
                started.elapsed().as_secs_f64(),
            );
        }
    }
    let started = Instant::now();
    gpu.sorter
        .sort(&mut gpu.input, &mut gpu.scratch, count, true, cancelled)?;
    if profile {
        eprintln!(
            "pos2_cuda_resident sort_3 seconds={:.3} work={}",
            started.elapsed().as_secs_f64(),
            limits.max_work - remaining_work,
        );
    }
    let Resident {
        context,
        stream,
        input,
        scratch,
        index,
        counters,
        sorter,
        module,
        ..
    } = gpu;
    drop(scratch);
    drop(index);
    drop(counters);
    drop(sorter);
    drop(module);
    context.check_err().map_err(gpu_error)?;
    check_cancelled(cancelled)?;
    Ok(Some(DevicePlot {
        context,
        stream,
        entries: input,
        count,
        params: params.clone(),
        table_counts,
    }))
}

pub(super) fn build(
    context: Arc<CudaContext>,
    params: &ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<Option<CompactPlot>, Error> {
    build_device(context, params, limits, cancelled)?
        .map(|plot| plot.download(cancelled))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_resident_memory_preflight_accounts_for_all_device_storage() {
        assert!(memory_plan(INITIAL_ENTRIES - 1).is_err());
        for max_entries in [INITIAL_ENTRIES, 310_000_000, usize::MAX] {
            let plan = memory_plan(max_entries).unwrap();
            let limits = PlotLimits {
                memory_bytes: plan.managed_bytes,
                max_entries,
                max_work: u64::MAX,
            };
            let required = plan.device_bytes + DEVICE_HEADROOM_BYTES;
            assert!(fits(&plan, limits, required).unwrap());
            assert!(!fits(&plan, limits, required - 1).unwrap());
            assert!(
                !fits(
                    &plan,
                    PlotLimits {
                        memory_bytes: limits.memory_bytes - 1,
                        ..limits
                    },
                    required
                )
                .unwrap()
            );
            assert!(plan.managed_bytes < 12 << 30);
            assert!(plan.device_bytes > plan.capacity as u64 * 32 + INDEX_BYTES);
            assert!(plan.capacity >= INITIAL_ENTRIES);
            assert!(plan.capacity <= max_entries);
            let download_device_peak = plan.capacity as u64 * 24;
            let download_host_and_device_peak = plan.capacity as u64 * 16;
            assert!(download_device_peak < plan.managed_bytes);
            assert!(download_host_and_device_peak < plan.managed_bytes);
        }
    }

    #[test]
    fn full_resident_work_charge_is_bounded_and_cancellable() {
        let mut remaining = 17;
        charge(&mut remaining, 17, &AtomicBool::new(false)).unwrap();
        assert_eq!(remaining, 0);
        assert!(charge(&mut remaining, 1, &AtomicBool::new(false)).is_err());
        assert_eq!(remaining, 0);
        let error = charge(&mut remaining, 0, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
    }

    #[test]
    #[ignore = "requires an explicitly selected CUDA device"]
    fn full_resident_fragment_download_matches_sorted_gpu_records() -> Result<(), Error> {
        let ordinal = std::env::var("DGX_CUDA_TEST_DEVICE")
            .map_err(|_| Error::other("set DGX_CUDA_TEST_DEVICE explicitly for CUDA"))?
            .parse::<usize>()
            .map_err(|_| Error::other("invalid CUDA device ordinal"))?;
        let context = CudaContext::new(ordinal).map_err(gpu_error)?;
        let stream = context.default_stream();
        let count = 4097;
        let expected: Vec<_> = (0..count).map(|position| (position as u64) << 32).collect();
        let entries: Vec<_> = expected
            .iter()
            .map(|fragment| [*fragment as u32, (*fragment >> 32) as u32, 19, 23])
            .collect();
        let entries = DeviceBuffer::from_host(&stream, &entries).map_err(gpu_error)?;
        let params = ProofParams::new([29; 32].into(), 28, 2, false)?;
        let device = DevicePlot {
            context,
            stream,
            entries,
            count,
            params,
            table_counts: [INITIAL_ENTRIES, count, count, count],
        };
        let actual = device.download(&AtomicBool::new(false))?;
        assert_eq!(actual.fragments(), expected);
        Ok(())
    }
}
