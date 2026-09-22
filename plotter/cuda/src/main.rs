use clap::Parser;
use cuda_core::simt::LaunchConfig;
use cuda_core::{CudaContext, DeviceBuffer};
use cuda_device::{DisjointSlice, SharedArray, cuda_module, kernel, thread};
use dg_xch_pos2::{
    compute::{CpuHasher, HashEngine},
    params::ProofParams,
    plotting::{NativePlot, PlotLimits, Witness},
};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

mod full_resident;
mod packing;
mod packing_kernels;
mod radix;
mod radix_kernels;
mod resident;
mod resident_kernels;

#[path = "../../../proof_of_space/pos2/src/device.rs"]
mod device;
use device::{Config, Record};

unsafe impl cuda_core::DeviceCopy for Record {}
unsafe impl cuda_core::DeviceCopy for Config {}

#[cuda_module]
mod kernels {
    use super::*;

    #[kernel]
    pub fn hash_batch(
        keys: [[u32; 4]; 2],
        input: &[[u32; 4]],
        rounds: u32,
        mut output: DisjointSlice<[u32; 4]>,
    ) {
        static mut TABLE: SharedArray<u32, 256> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut TABLE) };
        let mut table_index = thread::threadIdx_x() as usize;
        while table_index < 256 {
            unsafe {
                shared
                    .add(table_index)
                    .write(device::AES_TABLE[table_index]);
            }
            table_index += thread::blockDim_x() as usize;
        }
        thread::sync_threads();
        let table = unsafe { &*shared.cast::<[u32; 256]>() };
        let index = thread::index_1d();
        let position = index.get();
        if let Some(slot) = output.get_mut(index) {
            *slot = device::hash_with_keys(keys, input[position], rounds, table);
        }
    }

    #[kernel]
    pub fn generate(config: Config, mut output: DisjointSlice<Record>) {
        let index = thread::index_1d();
        let position = index.get();
        if let Some(slot) = output.get_mut(index) {
            *slot = device::generate(config, position as u32);
        }
    }

    #[kernel]
    pub fn sort_pass(
        input: &[Record],
        mut output: DisjointSlice<Record>,
        distance: u32,
        width: u32,
        final_table: u32,
    ) {
        let index = thread::index_1d();
        let position = index.get();
        if let Some(slot) = output.get_mut(index) {
            let partner = position ^ distance as usize;
            let left = input[position];
            let right = input[partner];
            let choose_minimum =
                (position & width as usize == 0) == (position & distance as usize == 0);
            *slot = if device::less(right, left, final_table != 0) == choose_minimum {
                right
            } else {
                left
            };
        }
    }

    #[kernel]
    pub fn counts(
        config: Config,
        table: u32,
        input: &[Record],
        length: u32,
        max_work: u32,
        mut output: DisjointSlice<u32>,
    ) {
        let index = thread::index_1d();
        let position = index.get();
        if let Some(slot) = output.get_mut(index) {
            *slot = device::scan(
                config,
                table,
                input,
                length as usize,
                position,
                u32::MAX,
                max_work,
            )
            .1;
        }
    }

    #[kernel]
    pub fn emit(
        config: Config,
        table: u32,
        input: &[Record],
        length: u32,
        offsets: &[u32],
        max_work: u32,
        total: u32,
        mut output: DisjointSlice<Record>,
    ) {
        let index = thread::index_1d();
        let position = index.get();
        if let Some(slot) = output.get_mut(index) {
            if position < total as usize {
                let mut lower = 0usize;
                let mut upper = length as usize;
                while lower < upper {
                    let middle = lower + (upper - lower) / 2;
                    if offsets[middle + 1] <= position as u32 {
                        lower = middle + 1;
                    } else {
                        upper = middle;
                    }
                }
                *slot = device::scan(
                    config,
                    table,
                    input,
                    length as usize,
                    lower,
                    position as u32 - offsets[lower],
                    max_work,
                )
                .0;
            }
        }
    }
}

fn gpu_error(error: impl std::fmt::Display) -> Error {
    Error::other(format!("CUDA: {error}"))
}

struct CudaHasher {
    context: std::sync::Arc<CudaContext>,
    module: kernels::LoadedModule,
    keys: [[u32; 4]; 2],
    buffers: Option<(DeviceBuffer<[u32; 4]>, DeviceBuffer<[u32; 4]>)>,
}

impl CudaHasher {
    fn new(params: &ProofParams, ordinal: usize) -> Result<Self, Error> {
        let context = CudaContext::new(ordinal).map_err(gpu_error)?;
        let module = kernels::load(&context).map_err(gpu_error)?;
        let plot_id = *AsRef::<[u8; 32]>::as_ref(&params.plot_id());
        Ok(Self {
            context,
            module,
            buffers: None,
            keys: std::array::from_fn(|half| {
                std::array::from_fn(|word| {
                    let offset = half * 16 + word * 4;
                    u32::from_le_bytes([
                        plot_id[offset],
                        plot_id[offset + 1],
                        plot_id[offset + 2],
                        plot_id[offset + 3],
                    ])
                })
            }),
        })
    }
}

impl dg_xch_pos2::compute::HashEngine for CudaHasher {
    fn is_accelerated(&self) -> bool {
        true
    }

    fn build_compact(
        &mut self,
        params: &ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<dg_xch_pos2::compact::CompactPlot>, Error> {
        if let Some(plot) = full_resident::build(self.context.clone(), params, limits, cancelled)? {
            return Ok(Some(plot));
        }
        resident::build(self.context.clone(), params, limits, cancelled)
    }

    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        use dg_xch_pos2::compute::{GPU_BATCH_SIZE, check_cancelled};
        check_cancelled(cancelled)?;
        if inputs.len() > GPU_BATCH_SIZE || rounds == 0 || rounds > 1024 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid bounded CUDA AES batch",
            ));
        }
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        self.context.bind_to_thread().map_err(gpu_error)?;
        let stream = self.context.default_stream();
        let (mut input, mut output) = match self.buffers.take() {
            Some((mut input, output)) if input.len() == inputs.len() => {
                input.copy_from_host(&stream, inputs).map_err(gpu_error)?;
                (input, output)
            }
            buffers => {
                self.buffers = buffers;
                (
                    DeviceBuffer::from_host(&stream, inputs).map_err(gpu_error)?,
                    DeviceBuffer::<[u32; 4]>::zeroed(&stream, inputs.len()).map_err(gpu_error)?,
                )
            }
        };
        let step = (4_194_304 / inputs.len()).clamp(1, 1024) as u32;
        let mut remaining = rounds;
        while remaining > 0 {
            check_cancelled(cancelled)?;
            let count = step.min(remaining);
            unsafe {
                self.module.hash_batch(
                    &stream,
                    LaunchConfig::for_num_elems(inputs.len() as u32),
                    self.keys,
                    &input,
                    count,
                    &mut output,
                )
            }
            .map_err(gpu_error)?;
            std::mem::swap(&mut input, &mut output);
            remaining -= count;
            if remaining > 0 {
                stream.synchronize().map_err(gpu_error)?;
            }
        }
        let result = input.to_host_vec(&stream).map_err(gpu_error)?;
        if self
            .buffers
            .as_ref()
            .is_none_or(|(cached, _)| cached.len() < input.len())
        {
            self.buffers = Some((input, output));
        }
        check_cancelled(cancelled)?;
        Ok(result)
    }
}

fn build(
    params: ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    ordinal: usize,
) -> Result<NativePlot, Error> {
    dg_xch_pos2::compute::check_cancelled(cancelled)?;
    if params.strength() > 8 {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "full GPU tables are limited to strengths 2 through 8; use the default bounded compact pipeline",
        ));
    }
    let initial = 1u64 << params.k();
    if initial > u64::from(u32::MAX) || initial > limits.max_entries as u64 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "CUDA plot exceeds entry/index limits",
        ));
    }
    let context = CudaContext::new(ordinal).map_err(gpu_error)?;
    let stream = context.default_stream();
    let module = kernels::load(&context).map_err(gpu_error)?;
    let config = Config {
        plot_id: *AsRef::<[u8; 32]>::as_ref(&params.plot_id()),
        k: u32::from(params.k()),
        strength: u32::from(params.strength()),
        testnet: u32::from(params.is_testnet()),
    };
    let check_memory = |entries: usize| -> Result<(), Error> {
        let bytes = (entries as u64)
            .checked_mul((size_of::<Record>() * 3 + 8) as u64)
            .ok_or_else(|| Error::other("CUDA allocation overflow"))?;
        if bytes > limits.memory_bytes {
            return Err(Error::other("CUDA buffer memory budget exceeded"));
        }
        Ok(())
    };
    check_memory(initial as usize)?;
    let mut input = DeviceBuffer::<Record>::zeroed(&stream, initial as usize).map_err(gpu_error)?;
    unsafe {
        module.generate(
            &stream,
            LaunchConfig::for_num_elems(initial as u32),
            config,
            &mut input,
        )
    }
    .map_err(gpu_error)?;
    let mut length = initial as usize;
    let mut padded = length;
    let mut table_counts = [length, 0, 0, 0];
    let mut remaining = limits
        .max_work
        .checked_sub(initial)
        .ok_or_else(|| Error::other("CUDA work budget exceeded"))?;
    for table in 1..=4 {
        check_memory(padded)?;
        let mut scratch = DeviceBuffer::<Record>::zeroed(&stream, padded).map_err(gpu_error)?;
        let mut width = 2usize;
        while width <= padded {
            let mut distance = width / 2;
            while distance > 0 {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(Error::new(
                        ErrorKind::Interrupted,
                        "CUDA plotting cancelled",
                    ));
                }
                unsafe {
                    module.sort_pass(
                        &stream,
                        LaunchConfig::for_num_elems(padded as u32),
                        &input,
                        &mut scratch,
                        distance as u32,
                        width as u32,
                        u32::from(table == 4),
                    )
                }
                .map_err(gpu_error)?;
                std::mem::swap(&mut input, &mut scratch);
                distance /= 2;
            }
            width *= 2;
        }
        drop(scratch);
        if table == 4 {
            break;
        }
        let work_per_entry = (remaining / (length as u64 * 4)).min(u64::from(u32::MAX - 1)) as u32;
        if work_per_entry == 0 {
            return Err(Error::other("CUDA work budget exceeded"));
        }
        let mut counts = DeviceBuffer::<u32>::zeroed(&stream, length).map_err(gpu_error)?;
        unsafe {
            module.counts(
                &stream,
                LaunchConfig::for_num_elems(length as u32),
                config,
                table,
                &input,
                length as u32,
                work_per_entry,
                &mut counts,
            )
        }
        .map_err(gpu_error)?;
        let counts = counts.to_host_vec(&stream).map_err(gpu_error)?;
        let mut offsets = Vec::with_capacity(length + 1);
        offsets.push(0u32);
        let mut total = 0u32;
        for count in counts {
            if count == u32::MAX {
                return Err(Error::other("CUDA per-entry work budget exceeded"));
            }
            total = total
                .checked_add(count)
                .ok_or_else(|| Error::other("CUDA output count overflow"))?;
            offsets.push(total);
        }
        if total == 0 || total as usize > limits.max_entries {
            return Err(Error::other("CUDA output exceeds entry budget or is empty"));
        }
        let charged = (u64::from(total) + length as u64)
            .checked_mul(u64::from(work_per_entry))
            .ok_or_else(|| Error::other("CUDA work overflow"))?;
        remaining = remaining
            .checked_sub(charged)
            .ok_or_else(|| Error::other("CUDA work budget exceeded"))?;
        let next_padded = (total as usize)
            .checked_next_power_of_two()
            .ok_or_else(|| Error::other("CUDA sort size overflow"))?;
        if next_padded > u32::MAX as usize {
            return Err(Error::other("CUDA sort index limit exceeded"));
        }
        check_memory(next_padded.max(padded))?;
        let offsets = DeviceBuffer::from_host(&stream, &offsets).map_err(gpu_error)?;
        let mut output = DeviceBuffer::<Record>::zeroed(&stream, next_padded).map_err(gpu_error)?;
        unsafe {
            module.emit(
                &stream,
                LaunchConfig::for_num_elems(next_padded as u32),
                config,
                table,
                &input,
                length as u32,
                &offsets,
                work_per_entry,
                total,
                &mut output,
            )
        }
        .map_err(gpu_error)?;
        input = output;
        padded = next_padded;
        length = total as usize;
        table_counts[table as usize] = length;
    }
    let records = input.to_host_vec(&stream).map_err(gpu_error)?;
    let mut witnesses = Vec::with_capacity(length);
    for record in records.into_iter().take(length) {
        if record.valid != 1 {
            return Err(Error::other("CUDA emission failed"));
        }
        witnesses.push(Witness {
            fragment: record.fragment,
            xs: record.xs,
        });
    }
    NativePlot::from_witnesses(params, witnesses, table_counts, cancelled)
}

enum PlotOutput {
    Device(full_resident::DevicePlot),
    Host(dg_xch_pos2::compact::CompactPlot),
}

impl PlotOutput {
    #[cfg(test)]
    fn table_counts(&self) -> [usize; 4] {
        match self {
            Self::Device(plot) => plot.table_counts,
            Self::Host(plot) => plot.table_counts,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        output: &mut (impl std::io::Write + std::io::Seek),
        index: u16,
        meta_group: u8,
        memo: &[u8],
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        match self {
            Self::Device(plot) => packing::write(
                output,
                plot.context.clone(),
                plot.stream.clone(),
                &plot.entries,
                plot.count,
                &plot.params,
                index,
                meta_group,
                memo,
                memory_bytes,
                cancelled,
            ),
            Self::Host(plot) => dg_xch_plotter::format::write_compact(
                output, plot, index, meta_group, memo, cancelled,
            ),
        }
    }
}

#[derive(Parser)]
#[command(about = "Experimental native Rust CUDA PoS2 plotter; no CPU fallback")]
struct Arguments {
    #[arg(
        long,
        required_unless_present_any = ["prove_plot", "probe_device"],
        conflicts_with = "prove_plot"
    )]
    output: Option<PathBuf>,
    #[arg(long, required_unless_present_any = ["prove_plot", "probe_device"])]
    farmer_key: Option<String>,
    #[arg(long, conflicts_with_all = ["output", "prove_plot", "farmer_key", "pool_key", "contract", "challenge"])]
    probe_device: bool,
    #[arg(long, requires = "challenge")]
    prove_plot: Option<PathBuf>,
    #[arg(long, requires = "prove_plot")]
    challenge: Option<String>,
    #[arg(long, requires = "prove_plot")]
    quality: Option<String>,
    #[arg(
        long,
        required_unless_present_any = ["contract", "prove_plot", "probe_device"],
        conflicts_with = "contract"
    )]
    pool_key: Option<String>,
    #[arg(long)]
    contract: Option<String>,
    #[arg(long, default_value_t = 18)]
    k: u8,
    #[arg(long, default_value_t = 2)]
    strength: u8,
    #[arg(long, default_value_t = 0)]
    index: u16,
    #[arg(long, default_value_t = 0)]
    meta_group: u8,
    #[arg(long, default_value_t = 0)]
    device: usize,
    #[arg(long)]
    testnet: bool,
    #[arg(long)]
    full_gpu_tables: bool,
    #[arg(long, default_value_t = 512)]
    memory_mib: u64,
    #[arg(long, default_value_t = 4_194_304)]
    max_entries: usize,
    #[arg(long, default_value_t = 1_000_000_000)]
    max_work: u64,
}

fn decode<const SIZE: usize>(value: &str) -> Result<[u8; SIZE], Error> {
    hex::decode(value)
        .map_err(gpu_error)?
        .try_into()
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "wrong key/hash length"))
}

fn main() -> Result<(), Error> {
    let args = Arguments::parse();
    if args.probe_device {
        let params = ProofParams::new([0; 32].into(), 18, 2, false)?;
        let mut engine = CudaHasher::new(&params, args.device)?;
        let cancelled = AtomicBool::new(false);
        let input = [[0; 4]];
        if engine.hash(&input, 16, &cancelled)?
            != CpuHasher::new(&params).hash(&input, 16, &cancelled)?
        {
            return Err(Error::other(
                "CUDA probe kernel did not match native hashing",
            ));
        }
        let name = engine.context.device_name().map_err(gpu_error)?;
        println!("DGX_CUDA_DEVICE_V1\t{}\t{}", args.device, hex::encode(name));
        return Ok(());
    }
    let limits = PlotLimits {
        memory_bytes: args
            .memory_mib
            .checked_mul(1024 * 1024)
            .ok_or_else(|| Error::other("memory budget overflow"))?,
        max_entries: args.max_entries,
        max_work: args.max_work,
    };
    if let Some(path) = &args.prove_plot {
        let cancelled = AtomicBool::new(false);
        let mut plot =
            dg_xch_plotter::reader::PlotReader::open(path, args.testnet, limits.memory_bytes)?;
        let mut engine = CudaHasher::new(plot.params(), args.device)?;
        let challenge = decode::<32>(
            args.challenge
                .as_deref()
                .ok_or_else(|| Error::other("challenge required"))?,
        )?
        .into();
        let requested_quality = args.quality.as_deref().map(decode::<32>).transpose()?;
        let mut matched_quality = false;
        for chain in plot.qualities(
            challenge,
            dg_xch_pos2::chainer::SearchLimits {
                max_hashes: args.max_work,
                max_results: 1024,
            },
            &cancelled,
        )? {
            let quality = dg_xch_pos2::quality::quality_hash(&chain.fragments, plot.info.strength);
            if requested_quality
                .as_ref()
                .is_some_and(|requested| quality.const_bytes() != *requested)
            {
                continue;
            }
            matched_quality = true;
            println!(
                "quality={} proof={}",
                quality,
                hex::encode(plot.prove_with_engine(
                    &chain,
                    challenge,
                    limits,
                    &cancelled,
                    &mut engine
                )?)
            );
            if requested_quality.is_some() {
                break;
            }
        }
        if requested_quality.is_some() && !matched_quality {
            return Err(Error::new(
                ErrorKind::NotFound,
                "requested proof quality is not present in this plot challenge",
            ));
        }
        return Ok(());
    }
    let pool = match (args.pool_key, args.contract) {
        (Some(key), None) => dg_xch_plotter::PoolBinding::PublicKey(decode(&key)?),
        (None, Some(hash)) => dg_xch_plotter::PoolBinding::Contract(decode(&hash)?),
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "choose one pool binding",
            ));
        }
    };
    let request = dg_xch_plotter::PlotRequest {
        farmer_public_key: decode(
            args.farmer_key
                .as_deref()
                .ok_or_else(|| Error::other("farmer key required"))?,
        )?,
        pool,
        k: args.k,
        strength: args.strength,
        index: args.index,
        meta_group: args.meta_group,
        testnet: args.testnet,
    };
    let output = args
        .output
        .as_deref()
        .ok_or_else(|| Error::other("output required"))?;
    let cancelled = AtomicBool::new(false);
    let info = if args.full_gpu_tables {
        dg_xch_plotter::create_with_engine(
            &request,
            output,
            limits,
            &cancelled,
            |params, limits, cancelled| build(params, limits, cancelled, args.device),
        )?
    } else {
        dg_xch_plotter::create_with_writer(&request, output, &cancelled, |params, output, memo| {
            let mut engine = CudaHasher::new(&params, args.device)?;
            let plot = match full_resident::build_device(
                engine.context.clone(),
                &params,
                limits,
                &cancelled,
            )? {
                Some(plot) => PlotOutput::Device(plot),
                None => PlotOutput::Host(dg_xch_pos2::compact::CompactPlot::build_with_engine(
                    params,
                    limits,
                    &cancelled,
                    &mut engine,
                )?),
            };
            plot.write(
                output,
                request.index,
                request.meta_group,
                memo,
                limits.memory_bytes,
                &cancelled,
            )
        })?
    };
    println!("{info:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TimedHasher {
        inner: CudaHasher,
        elapsed: std::time::Duration,
        batches: u64,
        inputs: u64,
        evaluations: u64,
    }

    impl HashEngine for TimedHasher {
        fn is_accelerated(&self) -> bool {
            self.inner.is_accelerated()
        }

        fn build_compact(
            &mut self,
            params: &ProofParams,
            limits: PlotLimits,
            cancelled: &AtomicBool,
        ) -> Result<Option<dg_xch_pos2::compact::CompactPlot>, Error> {
            self.inner.build_compact(params, limits, cancelled)
        }

        fn hash(
            &mut self,
            inputs: &[[u32; 4]],
            rounds: u32,
            cancelled: &AtomicBool,
        ) -> Result<Vec<[u32; 4]>, Error> {
            let started = std::time::Instant::now();
            let result = self.inner.hash(inputs, rounds, cancelled);
            self.elapsed += started.elapsed();
            self.batches += 1;
            self.inputs += inputs.len() as u64;
            self.evaluations += inputs.len() as u64 * u64::from(rounds / 16);
            result
        }
    }

    #[test]
    #[ignore = "full k28 CUDA plotting benchmark; requires explicit paths, device and release mode"]
    fn k28_plotting() -> Result<(), Error> {
        use dg_xch_pos2::compact::CompactPlot;
        use std::fs::File;
        use std::io::Read;
        use std::path::Path;
        use std::time::{Duration, Instant};

        if cfg!(debug_assertions) {
            return Err(Error::other("run plotting benchmarks in release mode"));
        }
        let source =
            PathBuf::from(std::env::var_os("DGX_POS2_BENCHMARK_INPUT").ok_or_else(|| {
                Error::other("set DGX_POS2_BENCHMARK_INPUT to the benchmark seed")
            })?);
        let destination = PathBuf::from(
            std::env::var_os("DGX_POS2_BENCHMARK_OUTPUT")
                .ok_or_else(|| Error::other("set DGX_POS2_BENCHMARK_OUTPUT to a new plot path"))?,
        );
        if destination.exists() {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                "benchmark destination already exists",
            ));
        }
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let fixture = dg_xch_plotter::inspect(&source)?;
        if fixture.k != 28 || fixture.strength != 2 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "benchmark input must be a mainnet k28 strength-2 plot",
            ));
        }
        let mut source = File::open(source)?;
        let mut header = [0; 43];
        source.read_exact(&mut header)?;
        let mut memo = vec![0; usize::from(header[42])];
        source.read_exact(&mut memo)?;
        let params = ProofParams::new(fixture.plot_id.into(), 28, 2, false)?;
        let ordinal = std::env::var("DGX_POS2_BENCHMARK_DEVICE")
            .map_err(|_| Error::other("set DGX_POS2_BENCHMARK_DEVICE explicitly for CUDA"))?
            .parse::<usize>()
            .map_err(|_| Error::other("invalid CUDA device ordinal"))?;
        let limits = PlotLimits {
            memory_bytes: 12 * 1024 * 1024 * 1024,
            max_entries: 310_000_000,
            max_work: 100_000_000_000,
        };
        let cancelled = AtomicBool::new(false);
        let total_started = Instant::now();
        let engine = CudaHasher::new(&params, ordinal)?;
        println!(
            "benchmark_device={} ordinal={ordinal}",
            engine.context.device_name().map_err(gpu_error)?
        );
        let setup_seconds = total_started.elapsed().as_secs_f64();
        let mut engine = TimedHasher {
            inner: engine,
            elapsed: Duration::ZERO,
            batches: 0,
            inputs: 0,
            evaluations: 0,
        };
        let build_started = Instant::now();
        let plot = match full_resident::build_device(
            engine.inner.context.clone(),
            &params,
            limits,
            &cancelled,
        )? {
            Some(plot) => PlotOutput::Device(plot),
            None => PlotOutput::Host(CompactPlot::build_with_engine(
                params,
                limits,
                &cancelled,
                &mut engine,
            )?),
        };
        let build_seconds = build_started.elapsed().as_secs_f64();
        let write_started = Instant::now();
        let mut temporary = tempfile::Builder::new()
            .prefix(".pos2-benchmark-")
            .suffix(".partial")
            .tempfile_in(parent)?;
        plot.write(
            temporary.as_file_mut(),
            fixture.index,
            fixture.meta_group,
            &memo,
            limits.memory_bytes,
            &cancelled,
        )?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        let write_sync_seconds = write_started.elapsed().as_secs_f64();
        let total_seconds = total_started.elapsed().as_secs_f64();
        let info = dg_xch_plotter::inspect(&destination)?;
        assert_eq!(info.plot_id, fixture.plot_id);
        assert_eq!(info.k, 28);
        assert_eq!(info.strength, 2);
        println!(
            "benchmark backend=cuda k=28 strength=2 testnet=false plot_id={} bytes={} chunks={} table_counts={:?} memory_budget_bytes={} max_entries={} max_work={} rayon_threads={}",
            hex::encode(info.plot_id),
            info.file_bytes,
            info.chunks,
            plot.table_counts(),
            limits.memory_bytes,
            limits.max_entries,
            limits.max_work,
            std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "automatic".into())
        );
        println!(
            "benchmark_seconds setup={setup_seconds:.6} build={build_seconds:.6} hash_profiled={} hash_calls={:.6} wall_outside_hash_calls={:.6} write_sync={write_sync_seconds:.6} total={total_seconds:.6}",
            engine.batches != 0,
            engine.elapsed.as_secs_f64(),
            build_seconds - engine.elapsed.as_secs_f64()
        );
        println!(
            "benchmark_work batches={} inputs={} sixteen_round_evaluations={} output={}",
            engine.batches,
            engine.inputs,
            engine.evaluations,
            destination.display()
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires NVIDIA GPU; compact plotting and fragment reconstruction"]
    fn gpu_compact_plotting_and_fragment_proving_match_cpu() {
        use dg_xch_pos2::{
            chainer::SearchLimits,
            compact::CompactPlot,
            compute::{CpuHasher, Work},
            solver,
            validator::ProofValidator,
        };
        let cancelled = AtomicBool::new(false);
        for (testnet, strength) in [(false, 2), (true, 3)] {
            let params = ProofParams::new([42; 32].into(), 18, strength, testnet).unwrap();
            let limits = PlotLimits::default();
            let cpu = NativePlot::build(params.clone(), limits, &cancelled).unwrap();
            let mut engine = CudaHasher::new(&params, 0).unwrap();
            let gpu =
                CompactPlot::build_with_engine(params.clone(), limits, &cancelled, &mut engine)
                    .unwrap();
            assert_eq!(cpu.table_counts, gpu.table_counts);
            assert!(
                cpu.witnesses()
                    .iter()
                    .map(|witness| witness.fragment)
                    .eq(gpu.fragments().iter().copied())
            );
            let mut found = false;
            for attempt in 0u16..256 {
                let mut challenge = [0; 32];
                challenge[..2].copy_from_slice(&attempt.to_le_bytes());
                let challenge = challenge.into();
                let chains = cpu
                    .qualities(
                        challenge,
                        SearchLimits {
                            max_hashes: 10_000_000,
                            max_results: 1024,
                        },
                        &cancelled,
                    )
                    .unwrap();
                if let Some(chain) = chains.first() {
                    let proof = solver::solve_with_engine(
                        &params,
                        chain,
                        challenge,
                        limits,
                        &cancelled,
                        &mut engine,
                    )
                    .unwrap();
                    assert_eq!(
                        ProofValidator::new(params.clone())
                            .unwrap()
                            .validate_packed_proof(&proof, challenge),
                        Some(chain.fragments)
                    );
                    found = true;
                    break;
                }
            }
            assert!(found);
            let input = [[1, 2, 3, 4]; 17];
            let expected = Work::new(limits, &cancelled)
                .hash(&mut CpuHasher::new(&params), &input, 2048)
                .unwrap();
            let actual = Work::new(limits, &cancelled)
                .hash(&mut engine, &input, 2048)
                .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn device_probe_needs_no_plot_keys_or_output() {
        let arguments =
            Arguments::try_parse_from(["plotter", "--probe-device", "--device", "2"]).unwrap();
        assert!(arguments.probe_device);
        assert_eq!(arguments.device, 2);
        assert!(
            Arguments::try_parse_from(["plotter", "--probe-device", "--output", "plot"]).is_err()
        );
    }

    #[test]
    #[ignore = "requires NVIDIA GPU and a cargo oxide test build"]
    fn gpu_reconstructed_plot_proves_cpu_created_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cpu.plot");
        let cancelled = AtomicBool::new(false);
        let key = decode("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb").unwrap();
        let request = dg_xch_plotter::PlotRequest {
            farmer_public_key: key,
            pool: dg_xch_plotter::PoolBinding::Contract([9; 32]),
            k: 18,
            strength: 2,
            index: 7,
            meta_group: 2,
            testnet: false,
        };
        let limits = PlotLimits {
            max_work: 1_000_000_000,
            ..Default::default()
        };
        dg_xch_plotter::create(&request, &path, limits, &cancelled).unwrap();
        let gpu = dg_xch_plotter::proving::ReconstructedPlot::open_with_engine(
            &path,
            false,
            limits,
            &cancelled,
            |params, limits, cancelled| build(params, limits, cancelled, 0),
        )
        .unwrap();
        let cpu =
            dg_xch_plotter::proving::ReconstructedPlot::open(&path, false, limits, &cancelled)
                .unwrap();
        let validator = dg_xch_pos2::validator::ProofValidator::new(
            ProofParams::new(gpu.info.plot_id.into(), 18, 2, false).unwrap(),
        )
        .unwrap();
        let search = dg_xch_pos2::chainer::SearchLimits {
            max_hashes: 10_000_000,
            max_results: 1024,
        };
        let mut found = false;
        for counter in 0u32..128 {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&counter.to_le_bytes());
            let challenge = bytes.into();
            let chains = gpu.qualities(challenge, search, &cancelled).unwrap();
            assert_eq!(
                chains,
                cpu.qualities(challenge, search, &cancelled).unwrap()
            );
            for chain in chains {
                let proof = gpu.prove(&chain, challenge).unwrap();
                assert_eq!(proof, cpu.prove(&chain, challenge).unwrap());
                assert!(validator.validate_packed_proof(&proof, challenge).is_some());
                found = true;
            }
            if found {
                break;
            }
        }
        assert!(
            found,
            "test must validate at least one GPU-reconstructed proof"
        );
    }

    #[test]
    #[ignore = "requires an NVIDIA GPU and a cargo oxide test build"]
    fn gpu_tables_match_cpu_witnesses() {
        let cancelled = AtomicBool::new(false);
        let limits = PlotLimits {
            max_work: 1_000_000_000,
            ..PlotLimits::default()
        };
        for testnet in [false, true] {
            for strength in [2, 3] {
                let params = ProofParams::new([7; 32].into(), 18, strength, testnet).unwrap();
                let gpu = build(params.clone(), limits, &cancelled, 0).unwrap();
                let cpu = NativePlot::build(params, limits, &cancelled).unwrap();
                assert_eq!(gpu.table_counts, cpu.table_counts);
                assert_eq!(gpu.witnesses(), cpu.witnesses());
            }
        }
    }

    #[test]
    #[ignore = "requires NVIDIA GPU and DGX_POS2_REFERENCE_BIN"]
    fn gpu_file_matches_chia_byte_for_byte() {
        let reference =
            std::env::var_os("DGX_POS2_REFERENCE_BIN").expect("set DGX_POS2_REFERENCE_BIN");
        let key = decode("97f1d3a73197d7942695638c4fa9ac0fc3688c4f9774b905a14e3a3f171bac586c55e83ff97a1aeffb3af00adb22c6bb").unwrap();
        for portable in [false, true] {
            for testnet in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let actual = directory.path().join("gpu.plot");
                let expected = directory.path().join("chia.plot");
                let request = dg_xch_plotter::PlotRequest {
                    farmer_public_key: key,
                    pool: if portable {
                        dg_xch_plotter::PoolBinding::Contract([9; 32])
                    } else {
                        dg_xch_plotter::PoolBinding::PublicKey(key)
                    },
                    k: 18,
                    strength: 2,
                    index: 256,
                    meta_group: 3,
                    testnet,
                };
                dg_xch_plotter::create_compact_with_engine(
                    &request,
                    &actual,
                    PlotLimits {
                        max_work: 1_000_000_000,
                        ..PlotLimits::default()
                    },
                    &AtomicBool::new(false),
                    |params, limits, cancelled| {
                        let mut engine = CudaHasher::new(&params, 0)?;
                        dg_xch_pos2::compact::CompactPlot::build_with_engine(
                            params,
                            limits,
                            cancelled,
                            &mut engine,
                        )
                    },
                )
                .unwrap();
                assert!(
                    std::process::Command::new(&reference)
                        .arg(&actual)
                        .arg(&expected)
                        .arg(testnet.to_string())
                        .status()
                        .unwrap()
                        .success()
                );
                assert!(
                    std::fs::read(actual).unwrap() == std::fs::read(expected).unwrap(),
                    "GPU file differs from Chia: portable={portable} testnet={testnet}"
                );
            }
        }
    }
}
