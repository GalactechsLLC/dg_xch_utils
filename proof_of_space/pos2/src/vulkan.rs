use crate::device::{self, Config, Record};
use crate::params::ProofParams;
use crate::plotting::{NativePlot, PlotLimits, Witness};
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const BATCH_SIZE: usize = crate::compute::BATCH_SIZE;
const GPU_BATCH_SIZE: usize = crate::compute::GPU_BATCH_SIZE;
const BUFFER_BYTES: u64 = (GPU_BATCH_SIZE * size_of::<[u32; 4]>()) as u64;
const BATCH_MEMORY_BYTES: u64 = crate::compute::SCRATCH_BYTES;
const HASH_TIMEOUT: Duration = Duration::from_secs(30);
const CANCELLATION_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct AdapterInfo {
    pub ordinal: usize,
    pub name: String,
    pub vendor: u32,
    pub device: u32,
}

pub(crate) fn instance() -> Option<wgpu::Instance> {
    if !wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::VULKAN) {
        return None;
    }
    Some(wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    }))
}

pub(crate) fn hardware_adapters(instance: &wgpu::Instance) -> Vec<wgpu::Adapter> {
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN))
        .into_iter()
        .filter(|adapter| {
            matches!(
                adapter.get_info().device_type,
                wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
            )
        })
        .collect()
}

pub fn adapters() -> Vec<AdapterInfo> {
    let Some(instance) = instance() else {
        return Vec::new();
    };
    hardware_adapters(&instance)
        .into_iter()
        .enumerate()
        .map(|(ordinal, adapter)| {
            let info = adapter.get_info();
            AdapterInfo {
                ordinal,
                name: info.name,
                vendor: info.vendor,
                device: info.device,
            }
        })
        .collect()
}

fn cancelled_check(cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(Error::new(
            ErrorKind::Interrupted,
            "Vulkan plotting cancelled",
        ));
    }
    Ok(())
}

fn gpu_error(error: impl std::fmt::Display) -> Error {
    Error::other(format!("Vulkan compute: {error}"))
}

fn allocation<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| Error::other("Vulkan host allocation failed"))?;
    Ok(values)
}

fn capacity(params: &ProofParams, limits: PlotLimits) -> Result<usize, Error> {
    let available = limits.memory_bytes.saturating_sub(BATCH_MEMORY_BYTES);
    let capacity = limits
        .max_entries
        .min((available / (2 * size_of::<Record>()) as u64) as usize);
    let initial = 1u64 << params.k();
    if initial > capacity as u64 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Vulkan in-memory plot exceeds memory, entry or index limits",
        ));
    }
    if limits.max_work < initial {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Vulkan work budget exceeded",
        ));
    }
    Ok(capacity)
}

pub struct Hasher {
    ordinal: usize,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    bindings: wgpu::BindGroup,
    configuration: wgpu::Buffer,
    inputs: wgpu::Buffer,
    outputs: wgpu::Buffer,
    readback: wgpu::Buffer,
    failure: Arc<Mutex<Option<String>>>,
    key_words: [u32; 8],
}

impl Hasher {
    pub fn for_params(params: &ProofParams, ordinal: usize) -> Result<Self, Error> {
        Self::new(crate::compute::config(params), ordinal)
    }

    fn new(config: Config, ordinal: usize) -> Result<Self, Error> {
        let instance = instance().ok_or_else(|| {
            Error::new(
                ErrorKind::Unsupported,
                "Vulkan backend is not supported on this target",
            )
        })?;
        let adapter = hardware_adapters(&instance)
            .into_iter()
            .nth(ordinal)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    "requested hardware Vulkan adapter unavailable; CPU/software fallback is disabled",
                )
            })?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("PoS2 Vulkan hashing"),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(gpu_error)?;
        let failure = Arc::new(Mutex::new(None));
        let error_slot = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut failure) = error_slot.lock() {
                *failure = Some(format!("{error}"));
            }
        }));
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PoS2 AES WGSL"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("vulkan_aes.wgsl"),
                    "\n",
                    include_str!("vulkan.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("PoS2 AES hashing"),
            layout: None,
            module: &shader,
            entry_point: Some("hash"),
            compilation_options: Default::default(),
            cache: None,
        });
        if let Some(error) = pollster::block_on(validation.pop()) {
            return Err(gpu_error(error));
        }
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let configuration = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 hash parameters"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: BUFFER_BYTES,
                usage,
                mapped_at_creation: false,
            })
        };
        let inputs = buffer(
            "PoS2 hash input",
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let outputs = buffer(
            "PoS2 hash output",
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let readback = buffer(
            "PoS2 hash readback",
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let aes_table = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 AES lookup table"),
            size: std::mem::size_of_val(&device::AES_TABLE) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&aes_table, 0, bytemuck::cast_slice(&device::AES_TABLE));
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PoS2 hash bindings"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: configuration.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: inputs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: outputs.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: aes_table.as_entire_binding(),
                },
            ],
        });
        for scope in [validation, internal, out_of_memory] {
            if let Some(error) = pollster::block_on(scope.pop()) {
                return Err(gpu_error(error));
            }
        }
        let key_words = std::array::from_fn(|index| {
            let offset = index * 4;
            u32::from_le_bytes([
                config.plot_id[offset],
                config.plot_id[offset + 1],
                config.plot_id[offset + 2],
                config.plot_id[offset + 3],
            ])
        });
        Ok(Self {
            ordinal,
            device,
            queue,
            pipeline,
            bindings,
            configuration,
            inputs,
            outputs,
            readback,
            failure,
            key_words,
        })
    }

    fn check_error(&self) -> Result<(), Error> {
        let failure = self
            .failure
            .lock()
            .map_err(|_| gpu_error("error channel poisoned"))?;
        if let Some(error) = failure.as_ref() {
            return Err(gpu_error(error));
        }
        Ok(())
    }

    fn hash(
        &self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        if inputs.len() > GPU_BATCH_SIZE || rounds < 16 || !rounds.is_multiple_of(16) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid Vulkan hash batch",
            ));
        }
        let step = ((4_194_304 / inputs.len() as u32).clamp(16, 1024) / 16) * 16;
        let count = rounds.min(step);
        let mut state = self.hash_once(inputs, count, cancelled)?;
        let mut remaining = rounds - count;
        while remaining > 0 {
            let count = remaining.min(step);
            state = self.hash_once(&state, count, cancelled)?;
            remaining -= count;
        }
        Ok(state)
    }

    fn hash_once(
        &self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        cancelled_check(cancelled)?;
        self.check_error()?;
        if inputs.is_empty() || inputs.len() > GPU_BATCH_SIZE || !(16..=1024).contains(&rounds) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid Vulkan hash batch",
            ));
        }
        let mut values = allocation(inputs.len())?;
        let mut parameters = [0u32; 12];
        parameters[..8].copy_from_slice(&self.key_words);
        parameters[8] = rounds;
        parameters[9] = inputs.len() as u32;
        self.queue
            .write_buffer(&self.configuration, 0, bytemuck::cast_slice(&parameters));
        self.queue
            .write_buffer(&self.inputs, 0, bytemuck::cast_slice(inputs));
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PoS2 bounded hashing batch"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PoS2 AES hashing"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.dispatch_workgroups((inputs.len() as u32).div_ceil(64), 1, 1);
        }
        let byte_count = std::mem::size_of_val(inputs) as u64;
        encoder.copy_buffer_to_buffer(&self.outputs, 0, &self.readback, 0, byte_count);
        let submission_index = self.queue.submit([encoder.finish()]);
        self.check_error()?;
        let slice = self.readback.slice(..byte_count);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let started = Instant::now();
        let mapped = loop {
            if let Err(error) = cancelled_check(cancelled).and_then(|()| self.check_error()) {
                break Err(error);
            }
            let Some(remaining) = HASH_TIMEOUT.checked_sub(started.elapsed()) else {
                break Err(Error::new(
                    ErrorKind::TimedOut,
                    "Vulkan hash batch timed out",
                ));
            };
            match self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission_index.clone()),
                timeout: Some(remaining.min(CANCELLATION_INTERVAL)),
            }) {
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(error) => break Err(gpu_error(error)),
            }
            match receiver.try_recv() {
                Ok(result) => break result.map_err(gpu_error),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err(gpu_error("readback channel closed"));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        };
        if let Err(error) = mapped {
            self.readback.unmap();
            return Err(error);
        }
        let output = match slice.get_mapped_range() {
            Ok(output) => output,
            Err(error) => {
                self.readback.unmap();
                return Err(gpu_error(error));
            }
        };
        for bytes in output.as_chunks::<16>().0 {
            values.push(std::array::from_fn(|index| {
                let offset = index * 4;
                u32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ])
            }));
        }
        drop(output);
        self.readback.unmap();
        self.check_error()?;
        Ok(values)
    }
}

impl crate::compute::HashEngine for Hasher {
    fn build_compact(
        &mut self,
        params: &ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<crate::compact::CompactPlot>, Error> {
        if let Some(plot) =
            crate::vulkan_full::build_device(params, self.ordinal, limits, cancelled)?
        {
            return plot.download(cancelled).map(Some);
        }
        crate::compact::gpu::build(params, limits, cancelled, self.ordinal)
    }

    fn is_accelerated(&self) -> bool {
        true
    }

    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        Hasher::hash(self, inputs, rounds, cancelled)
    }
}

struct Builder<'a> {
    config: Config,
    hasher: Hasher,
    capacity: usize,
    remaining: u64,
    cancelled: &'a AtomicBool,
}

impl Builder<'_> {
    fn hash(&mut self, inputs: &[[u32; 4]], rounds: u32) -> Result<Vec<[u32; 4]>, Error> {
        let cost = (inputs.len() as u64)
            .checked_mul(u64::from(rounds / 16))
            .ok_or_else(|| Error::other("Vulkan work calculation overflow"))?;
        self.remaining = self
            .remaining
            .checked_sub(cost)
            .ok_or_else(|| Error::other("Vulkan work budget exceeded"))?;
        self.hasher.hash(inputs, rounds, self.cancelled)
    }

    fn pair_batch(
        &mut self,
        table: u32,
        entries: &[Record],
        pairs: &mut Vec<(usize, usize)>,
        output: &mut Vec<Record>,
    ) -> Result<(), Error> {
        if pairs.is_empty() {
            return Ok(());
        }
        let mut inputs = allocation(pairs.len())?;
        for &(left, right) in pairs.iter() {
            inputs.push([
                entries[left].meta as u32,
                (entries[left].meta >> 32) as u32,
                entries[right].meta as u32,
                (entries[right].meta >> 32) as u32,
            ]);
        }
        let rounds = if table == 1 {
            16 << (self.config.strength - 2)
        } else {
            16
        };
        let hashes = self.hash(&inputs, rounds)?;
        for ((left, right), lanes) in pairs.drain(..).zip(hashes) {
            let result =
                device::pair_from_hash(self.config, table, entries[left], entries[right], lanes);
            if result.valid != 0 {
                if output.len() == self.capacity {
                    return Err(Error::other("Vulkan table entry budget exceeded"));
                }
                output.push(result);
            }
        }
        Ok(())
    }

    fn table(&mut self, table: u32, entries: &[Record]) -> Result<Vec<Record>, Error> {
        let keys = 1usize << if table == 1 { 2 } else { self.config.strength };
        let targets = entries
            .len()
            .checked_mul(keys)
            .ok_or_else(|| Error::other("Vulkan target count overflow"))?;
        let rounds = if table == 1 {
            16 << (self.config.strength - 2)
        } else {
            16
        };
        let mut output = allocation(self.capacity)?;
        let mut inputs = allocation(BATCH_SIZE)?;
        let mut pairs = allocation(BATCH_SIZE)?;
        for start in (0..targets).step_by(BATCH_SIZE) {
            cancelled_check(self.cancelled)?;
            inputs.clear();
            let end = (start + BATCH_SIZE).min(targets);
            for position in start..end {
                let entry = entries[position / keys];
                inputs.push([
                    table,
                    (position % keys) as u32,
                    entry.meta as u32,
                    (entry.meta >> 32) as u32,
                ]);
            }
            let hashes = self.hash(&inputs, rounds)?;
            for (position, lanes) in (start..end).zip(hashes) {
                let left = position / keys;
                let target = device::target_from_hash(
                    self.config,
                    table,
                    entries[left],
                    (position % keys) as u32,
                    lanes[0],
                );
                let first = entries.partition_point(|entry| entry.info < target);
                let last = entries.partition_point(|entry| entry.info <= target);
                for right in first..last {
                    pairs.push((left, right));
                    if pairs.len() == BATCH_SIZE {
                        self.pair_batch(table, entries, &mut pairs, &mut output)?;
                    }
                }
            }
        }
        self.pair_batch(table, entries, &mut pairs, &mut output)?;
        cancelled_check(self.cancelled)?;
        if table == 3 {
            output.sort_unstable_by_key(|entry| (entry.fragment, entry.xs));
        } else {
            output.sort_unstable_by_key(|entry| (entry.info, entry.meta, entry.xs));
        }
        cancelled_check(self.cancelled)?;
        Ok(output)
    }
}

pub fn build(
    params: ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    ordinal: usize,
) -> Result<NativePlot, Error> {
    cancelled_check(cancelled)?;
    let capacity = capacity(&params, limits)?;
    let config = Config {
        plot_id: *AsRef::<[u8; 32]>::as_ref(&params.plot_id()),
        k: u32::from(params.k()),
        strength: u32::from(params.strength()),
        testnet: u32::from(params.is_testnet()),
    };
    let mut builder = Builder {
        config,
        hasher: Hasher::new(config, ordinal)?,
        capacity,
        remaining: limits.max_work,
        cancelled,
    };
    let initial = 1usize << params.k();
    let mut entries = allocation(capacity)?;
    let mut inputs = allocation(BATCH_SIZE)?;
    for start in (0..initial).step_by(BATCH_SIZE) {
        inputs.clear();
        let end = (start + BATCH_SIZE).min(initial);
        for value in start..end {
            inputs.push([
                value as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                0,
                0,
                0,
            ]);
        }
        for (value, lanes) in (start..end).zip(builder.hash(&inputs, 16)?) {
            entries.push(device::generate_from_hash(config, value as u32, lanes));
        }
    }
    cancelled_check(cancelled)?;
    entries.sort_unstable_by_key(|entry| (entry.info, entry.meta, entry.xs));
    let mut counts = [entries.len(), 0, 0, 0];
    for table in 1..=3 {
        entries = builder.table(table, &entries)?;
        counts[table as usize] = entries.len();
    }
    let mut witnesses = allocation(entries.len())?;
    for entry in entries {
        witnesses.push(Witness {
            fragment: entry.fragment,
            xs: entry.xs,
        });
    }
    NativePlot::from_witnesses(params, witnesses, counts, cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_budgets_before_opening_device() {
        let params = ProofParams::new([1; 32].into(), 18, 2, false).unwrap();
        let limits = PlotLimits {
            memory_bytes: 1,
            ..Default::default()
        };
        assert!(build(params.clone(), limits, &AtomicBool::new(false), usize::MAX).is_err());
        assert_eq!(
            build(
                params,
                PlotLimits::default(),
                &AtomicBool::new(true),
                usize::MAX
            )
            .err()
            .unwrap()
            .kind(),
            ErrorKind::Interrupted
        );
    }

    #[test]
    #[ignore = "requires a real hardware Vulkan GPU; never falls back to a software adapter"]
    fn vulkan_hashes_match_native_for_all_supported_rounds() {
        let config = Config {
            plot_id: std::array::from_fn(|index| (index * 7) as u8),
            k: 18,
            strength: 2,
            testnet: 0,
        };
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .map(|value| value.parse::<usize>().expect("device ordinal"))
            .unwrap_or(0);
        eprintln!("Vulkan test device: {:?}", adapters().get(ordinal).unwrap());
        let hasher = Hasher::new(config, ordinal).unwrap();
        let inputs = [
            [0; 4],
            [u32::MAX; 4],
            [1, 3, 7, 11],
            [0x12345678, 0xabcdef01, 65537, 255],
        ];
        for rounds in [16, 32, 64, 128, 256, 512, 1024] {
            let actual = hasher
                .hash(&inputs, rounds, &AtomicBool::new(false))
                .unwrap();
            let expected: Vec<_> = inputs
                .into_iter()
                .map(|words| device::hash(config, words, rounds))
                .collect();
            assert_eq!(actual, expected);
        }
    }
}
