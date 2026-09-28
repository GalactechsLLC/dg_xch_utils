use crate::compact::resident::{INDEX_BYTES, TRANSFER_BYTES};
use crate::compact::{CompactPlot, Entry};
use crate::compute::{SCRATCH_BYTES, allocate, check_cancelled, config};
use crate::device;
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use crate::vulkan_radix;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

pub use crate::vulkan_packing::PackedReader;

pub(crate) const SHARD_ENTRIES: usize = 1 << 26;
pub(crate) const SHARDS: usize = 5;
const INITIAL_ENTRIES: usize = 1 << 28;
const GENERATION_BATCH: usize = 1 << 24;
const MATCHING_BATCH: usize = 1 << 23;
const TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct Context {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    failure: Arc<Mutex<Option<String>>>,
}

impl Context {
    pub(crate) fn new(ordinal: usize) -> Result<Option<Arc<Self>>, Error> {
        let instance = crate::vulkan::instance()
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "Vulkan backend unavailable"))?;
        let adapter = crate::vulkan::hardware_adapters(&instance)
            .into_iter()
            .nth(ordinal)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::NotFound,
                    "requested Vulkan plotting adapter unavailable",
                )
            })?;
        let limits = adapter.limits();
        let information = adapter.get_info();
        if !adapter.features().contains(wgpu::Features::SUBGROUP)
            || information.subgroup_min_size < 32
            || information.subgroup_max_size > 128
            || limits.max_storage_buffers_per_shader_stage < 14
            || limits.max_storage_buffer_binding_size < INDEX_BYTES
            || limits.max_buffer_size < INDEX_BYTES
            || limits.max_compute_workgroup_storage_size < 32 * 1024
            || limits.max_compute_invocations_per_workgroup < 256
            || limits.max_compute_workgroup_size_x < 256
        {
            return Ok(None);
        }
        let required_limits = wgpu::Limits {
            max_storage_buffers_per_shader_stage: 14,
            max_storage_buffer_binding_size: INDEX_BYTES,
            max_buffer_size: INDEX_BYTES,
            max_compute_workgroup_storage_size: 32 * 1024,
            max_compute_invocations_per_workgroup: 256,
            max_compute_workgroup_size_x: 256,
            ..wgpu::Limits::downlevel_defaults()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("PoS2 full-resident Vulkan plotting"),
            required_features: wgpu::Features::SUBGROUP,
            required_limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|error| Error::other(format!("Vulkan plotting device: {error}")))?;
        let failure = Arc::new(Mutex::new(None));
        let error_slot = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut failure) = error_slot.lock() {
                *failure = Some(error.to_string());
            }
        }));
        Ok(Some(Arc::new(Self {
            device,
            queue,
            failure,
        })))
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        let failure = self
            .failure
            .lock()
            .map_err(|_| Error::other("Vulkan plotting error lock poisoned"))?;
        match failure.as_ref() {
            Some(error) => Err(Error::other(format!(
                "Vulkan full-resident plotting: {error}"
            ))),
            None => Ok(()),
        }
    }

    pub(crate) fn encoder(&self, label: &str) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) })
    }

    pub(crate) fn wait(
        &self,
        submission: wgpu::SubmissionIndex,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        let started = Instant::now();
        loop {
            check_cancelled(cancelled)?;
            self.check()?;
            let remaining = TIMEOUT.checked_sub(started.elapsed()).ok_or_else(|| {
                Error::new(ErrorKind::TimedOut, "Vulkan plotting operation timed out")
            })?;
            match self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission.clone()),
                timeout: Some(remaining.min(Duration::from_millis(100))),
            }) {
                Ok(_) => {
                    check_cancelled(cancelled)?;
                    return self.check();
                }
                Err(wgpu::PollError::Timeout) => {}
                Err(error) => {
                    return Err(Error::other(format!("Vulkan plotting polling: {error}")));
                }
            }
        }
    }

    pub(crate) fn read(
        &self,
        buffer: &wgpu::Buffer,
        offset: u64,
        size: u64,
        cancelled: &AtomicBool,
    ) -> Result<Vec<u8>, Error> {
        check_cancelled(cancelled)?;
        self.check()?;
        if !offset.is_multiple_of(4)
            || !size.is_multiple_of(4)
            || size > TRANSFER_BYTES
            || offset
                .checked_add(size)
                .is_none_or(|end| end > buffer.size())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Vulkan readback exceeds its bounded allocation",
            ));
        }
        if size == 0 {
            return Ok(Vec::new());
        }
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 bounded Vulkan readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.encoder("PoS2 Vulkan readback command");
        encoder.copy_buffer_to_buffer(buffer, offset, &staging, 0, size);
        let submission = self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let result = (|| {
            self.wait(submission, cancelled)?;
            receiver
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| Error::other("Vulkan readback callback unavailable"))?
                .map_err(|error| Error::other(format!("Vulkan readback mapping: {error}")))?;
            let mapped = slice
                .get_mapped_range()
                .map_err(|error| Error::other(format!("Vulkan readback range: {error}")))?;
            let mut bytes = allocate(size as usize)?;
            bytes.extend_from_slice(&mapped);
            drop(mapped);
            self.check()?;
            check_cancelled(cancelled)?;
            Ok(bytes)
        })();
        staging.unmap();
        result
    }
}

pub(crate) fn allocate_entries(
    gpu: &Arc<Context>,
    capacity: usize,
    label: &str,
) -> Result<Vec<wgpu::Buffer>, Error> {
    if capacity > SHARDS * SHARD_ENTRIES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Vulkan table capacity exceeds shard addressing",
        ));
    }
    let buffers = (0..SHARDS)
        .map(|shard| {
            let count = capacity
                .saturating_sub(shard * SHARD_ENTRIES)
                .min(SHARD_ENTRIES);
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (count as u64 * size_of::<Entry>() as u64).max(16),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })
        .collect();
    gpu.check()?;
    Ok(buffers)
}

struct MemoryPlan {
    capacity: usize,
    device_bytes: u64,
    managed_bytes: u64,
}

fn memory_plan(max_entries: usize) -> Result<MemoryPlan, Error> {
    let tables = CompactPlot::memory_required(28, max_entries)?
        .checked_sub(SCRATCH_BYTES)
        .ok_or_else(|| Error::other("Vulkan table memory overflow"))?;
    let capacity = usize::try_from(tables / (2 * size_of::<Entry>() as u64))
        .map_err(|_| Error::other("Vulkan table capacity exceeds address space"))?;
    if capacity > SHARDS * SHARD_ENTRIES {
        return Err(Error::other(
            "Vulkan table capacity exceeds shader addressing",
        ));
    }
    let sort_bytes = vulkan_radix::scratch_bytes(capacity)?;
    let device_bytes = tables
        .checked_add(INDEX_BYTES)
        .and_then(|bytes| bytes.checked_add(128 + 16 + 1024 + SHARDS as u64 * 32))
        .and_then(|bytes| bytes.checked_add(sort_bytes))
        .ok_or_else(|| Error::other("Vulkan full-resident device memory overflow"))?;
    let managed_bytes = device_bytes
        .checked_add(SCRATCH_BYTES)
        .ok_or_else(|| Error::other("Vulkan full-resident managed memory overflow"))?;
    Ok(MemoryPlan {
        capacity,
        device_bytes,
        managed_bytes,
    })
}

fn charge(remaining: &mut u64, amount: u64, cancelled: &AtomicBool) -> Result<(), Error> {
    check_cancelled(cancelled)?;
    *remaining = remaining
        .checked_sub(amount)
        .ok_or_else(|| Error::other("Vulkan full-resident plotting work budget exceeded"))?;
    Ok(())
}

fn parameters(params: &ProofParams) -> [u32; 32] {
    let configuration = config(params);
    let mut words = [0; 32];
    for (destination, bytes) in words[..8]
        .iter_mut()
        .zip(configuration.plot_id.as_chunks::<4>().0)
    {
        *destination = u32::from_le_bytes(*bytes);
    }
    for (destination, keys) in words[8..24]
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(device::fragment_round_keys(configuration))
    {
        *destination = keys;
    }
    words[25] = u32::from(params.is_testnet());
    words
}

fn shader_source() -> String {
    let source = include_str!("vulkan_compact.wgsl")
        .replace("@binding(9)", "@binding(13)")
        .replace("@binding(8)", "@binding(12)")
        .replace("@binding(7)", "@binding(11)")
        .replace(
            "@group(0) @binding(6) var<storage, read_write> output: array<vec4<u32>>;",
            "@group(0) @binding(6) var<storage, read_write> output_first: array<vec4<u32>>;\n\
             @group(0) @binding(7) var<storage, read_write> output_second: array<vec4<u32>>;\n\
             @group(0) @binding(8) var<storage, read_write> output_third: array<vec4<u32>>;\n\
             @group(0) @binding(9) var<storage, read_write> output_fourth: array<vec4<u32>>;\n\
             @group(0) @binding(10) var<storage, read_write> output_fifth: array<vec4<u32>>;",
        )
        .replace(
            "    output[position] = value;",
            "    let absolute = configuration.padding + position;\n\
                 if absolute < configuration.padding {\n\
                     atomicOr(&counters.error_flags, 4u);\n\
                     return;\n\
                 }\n\
                 let offset = absolute & SHARD_MASK;\n\
                 switch absolute >> 26u {\n\
                     case 0u: { output_first[offset] = value; }\n\
                     case 1u: { output_second[offset] = value; }\n\
                     case 2u: { output_third[offset] = value; }\n\
                     case 3u: { output_fourth[offset] = value; }\n\
                     case 4u: { output_fifth[offset] = value; }\n\
                     default: { atomicOr(&counters.error_flags, 4u); }\n\
                 }",
        );
    format!(
        "{}\n{source}\n{EXTRACT_SHADER}",
        include_str!("vulkan_aes.wgsl")
    )
}

const EXTRACT_SHADER: &str = r#"
@compute @workgroup_size(64)
fn extract_fragments(
    @builtin(workgroup_id) group: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
    @builtin(local_invocation_index) local: u32,
) {
    let offset = flat_position(group, groups, local);
    if offset >= configuration.count {
        return;
    }
    let first_position = configuration.start + offset * 2u;
    if first_position < configuration.start || first_position >= configuration.input_len {
        atomicOr(&counters.error_flags, 4u);
        return;
    }
    let first = load_entry(first_position);
    var second = vec4<u32>(0u);
    if first_position + 1u < configuration.input_len {
        second = load_entry(first_position + 1u);
    }
    if (first.y | second.y) > 0x00ffffffu {
        atomicOr(&counters.error_flags, 4u);
        return;
    }
    output_first[offset] = vec4<u32>(first.xy, second.xy);
}
"#;

fn layout(gpu: &Context) -> wgpu::BindGroupLayout {
    let entries: Vec<_> = (0..14)
        .map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: if binding == 0 {
                    wgpu::BufferBindingType::Uniform
                } else {
                    wgpu::BufferBindingType::Storage {
                        read_only: matches!(binding, 1..=5 | 13),
                    }
                },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        })
        .collect();
    gpu.device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PoS2 full-resident table layout"),
            entries: &entries,
        })
}

fn pipelines(
    gpu: &Context,
    layout: &wgpu::BindGroupLayout,
    names: &[&str],
) -> Result<Vec<wgpu::ComputePipeline>, Error> {
    let validation = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PoS2 full-resident shared shader"),
            source: wgpu::ShaderSource::Wgsl(shader_source().into()),
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PoS2 full-resident pipeline layout"),
            bind_group_layouts: &[Some(layout)],
            immediate_size: 0,
        });
    let pipelines = names
        .iter()
        .map(|name| {
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: Some(name),
                    compilation_options: Default::default(),
                    cache: None,
                })
        })
        .collect();
    if let Some(error) = pollster::block_on(validation.pop()) {
        return Err(Error::other(format!(
            "Vulkan full-resident shader: {error}"
        )));
    }
    gpu.check()?;
    Ok(pipelines)
}

fn binding(
    gpu: &Context,
    layout: &wgpu::BindGroupLayout,
    buffers: &[&wgpu::Buffer],
) -> wgpu::BindGroup {
    let entries: Vec<_> = buffers
        .iter()
        .enumerate()
        .map(|(binding, buffer)| wgpu::BindGroupEntry {
            binding: binding as u32,
            resource: buffer.as_entire_binding(),
        })
        .collect();
    gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("PoS2 full-resident table bindings"),
        layout,
        entries: &entries,
    })
}

fn dispatch(
    gpu: &Context,
    pipeline: &wgpu::ComputePipeline,
    bindings: &wgpu::BindGroup,
    count: usize,
) -> Result<(), Error> {
    if count == 0 {
        return Ok(());
    }
    let count = u32::try_from(count).map_err(|_| Error::other("Vulkan dispatch count overflow"))?;
    let groups = count.div_ceil(64);
    let height = groups.div_ceil(65_535).max(1);
    let width = groups.div_ceil(height);
    let mut encoder = gpu.encoder("PoS2 full-resident compute command");
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("PoS2 full-resident compute"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bindings, &[]);
        pass.dispatch_workgroups(width, height, 1);
    }
    gpu.queue.submit([encoder.finish()]);
    gpu.check()
}

fn read_status(
    gpu: &Context,
    counters: &wgpu::Buffer,
    cancelled: &AtomicBool,
) -> Result<[u32; 4], Error> {
    let bytes = gpu.read(counters, 0, 16, cancelled)?;
    let status = bytemuck::try_pod_read_unaligned::<[u32; 4]>(&bytes)
        .map_err(|_| Error::other("Vulkan table returned invalid status bytes"))?;
    if status[2] != 0 {
        return Err(Error::other(format!(
            "Vulkan full-resident table rejected: flags={:#x}, outputs={}, pairs={}",
            status[2], status[0], status[1],
        )));
    }
    Ok(status)
}

struct Resident {
    gpu: Arc<Context>,
    inputs: Vec<wgpu::Buffer>,
    scratch: Vec<wgpu::Buffer>,
    layout: wgpu::BindGroupLayout,
    generate: wgpu::ComputePipeline,
    index: wgpu::ComputePipeline,
    matching: wgpu::ComputePipeline,
    configuration: wgpu::Buffer,
    buckets: wgpu::Buffer,
    counters: wgpu::Buffer,
    aes_table: wgpu::Buffer,
    sorter: vulkan_radix::Sorter,
    parameters: [u32; 32],
}

impl Resident {
    fn new(gpu: Arc<Context>, params: &ProofParams, capacity: usize) -> Result<Self, Error> {
        let inputs = allocate_entries(&gpu, capacity, "PoS2 full-resident input shard")?;
        let scratch = allocate_entries(&gpu, capacity, "PoS2 full-resident scratch shard")?;
        let layout = layout(&gpu);
        let mut pipelines = pipelines(&gpu, &layout, &["generate", "build_index", "match_table"])?;
        let matching = pipelines
            .pop()
            .ok_or_else(|| Error::other("Vulkan matching pipeline unavailable"))?;
        let index = pipelines
            .pop()
            .ok_or_else(|| Error::other("Vulkan index pipeline unavailable"))?;
        let generate = pipelines
            .pop()
            .ok_or_else(|| Error::other("Vulkan generation pipeline unavailable"))?;
        let configuration = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 full-resident parameters"),
            size: 128,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let storage = |label, size| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let buckets = storage("PoS2 full-resident dense index", INDEX_BYTES);
        let counters = storage("PoS2 full-resident counters", 16);
        let aes_table = storage("PoS2 full-resident AES table", 1024);
        gpu.queue
            .write_buffer(&aes_table, 0, bytemuck::cast_slice(&device::AES_TABLE));
        let sorter = vulkan_radix::Sorter::new(gpu.clone(), capacity)?;
        gpu.check()?;
        Ok(Self {
            gpu,
            inputs,
            scratch,
            layout,
            generate,
            index,
            matching,
            configuration,
            buckets,
            counters,
            aes_table,
            sorter,
            parameters: parameters(params),
        })
    }

    fn bindings(&self) -> wgpu::BindGroup {
        let buffers: Vec<_> = std::iter::once(&self.configuration)
            .chain(&self.inputs)
            .chain(&self.scratch)
            .chain([&self.buckets, &self.counters, &self.aes_table])
            .collect();
        binding(&self.gpu, &self.layout, &buffers)
    }

    fn configure(
        &mut self,
        table: u32,
        range: std::ops::Range<usize>,
        input_count: usize,
        output: std::ops::Range<usize>,
        pair_budget: u64,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        self.parameters[24] = table;
        for (word, value) in [
            (26, range.start),
            (27, range.len()),
            (28, input_count),
            (29, output.len()),
            (31, output.start),
        ] {
            self.parameters[word] = u32::try_from(value)
                .map_err(|_| Error::other("Vulkan table parameter overflow"))?;
        }
        self.parameters[30] = pair_budget.min(u64::from(u32::MAX)) as u32;
        self.gpu.queue.write_buffer(
            &self.configuration,
            0,
            bytemuck::cast_slice(&self.parameters),
        );
        self.gpu.queue.write_buffer(&self.counters, 0, &[0; 16]);
        self.gpu.check()
    }
}

pub struct DevicePlot {
    pub(crate) gpu: Arc<Context>,
    pub(crate) entries: Vec<wgpu::Buffer>,
    pub(crate) count: usize,
    pub(crate) capacity: usize,
    pub(crate) params: ProofParams,
    pub table_counts: [usize; 4],
}

impl DevicePlot {
    pub fn params(&self) -> &ProofParams {
        &self.params
    }

    pub fn download(self, cancelled: &AtomicBool) -> Result<CompactPlot, Error> {
        check_cancelled(cancelled)?;
        if self.count > self.capacity || self.entries.len() != SHARDS {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Vulkan final table exceeds device allocation",
            ));
        }
        let Self {
            gpu,
            entries,
            count,
            params,
            table_counts,
            ..
        } = self;
        let layout = layout(&gpu);
        let extraction = pipelines(&gpu, &layout, &["extract_fragments"])?
            .pop()
            .ok_or_else(|| Error::other("Vulkan fragment extraction pipeline unavailable"))?;
        let configuration = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 fragment extraction parameters"),
            size: 128,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 packed final fragments"),
            size: TRANSFER_BYTES,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let counters = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 fragment extraction counters"),
            size: 16,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let dummies: Vec<_> = (0..6)
            .map(|_| {
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("PoS2 unused fragment extraction binding"),
                    size: 1024,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let buffers: Vec<_> = std::iter::once(&configuration)
            .chain(&entries)
            .chain(std::iter::once(&output))
            .chain(dummies[..4].iter())
            .chain([&dummies[4], &counters, &dummies[5]])
            .collect();
        let bindings = binding(&gpu, &layout, &buffers);
        let mut output_values = allocate(count)?;
        let mut words = parameters(&params);
        words[28] = count as u32;
        let started = Instant::now();
        for start in (0..count).step_by(TRANSFER_BYTES as usize / 8) {
            check_cancelled(cancelled)?;
            let length = (TRANSFER_BYTES as usize / 8).min(count - start);
            words[26] = start as u32;
            words[27] = length.div_ceil(2) as u32;
            gpu.queue
                .write_buffer(&configuration, 0, bytemuck::cast_slice(&words));
            gpu.queue.write_buffer(&counters, 0, &[0; 16]);
            dispatch(&gpu, &extraction, &bindings, length.div_ceil(2))?;
            read_status(&gpu, &counters, cancelled)?;
            let bytes = gpu.read(&output, 0, length as u64 * 8, cancelled)?;
            output_values.extend(
                bytes
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|bytes| u64::from_le_bytes(*bytes)),
            );
        }
        if std::env::var_os("DGX_POS2_PROFILE").is_some() {
            eprintln!(
                "pos2_vulkan_resident download seconds={:.3} bytes={}",
                started.elapsed().as_secs_f64(),
                count as u64 * 8
            );
        }
        CompactPlot::from_sorted_fragments(params, output_values, table_counts, cancelled)
    }
}

pub fn build_device(
    params: &ProofParams,
    ordinal: usize,
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
            "insufficient Vulkan full-resident plotting work budget",
        ));
    }
    if plan.managed_bytes > limits.memory_bytes {
        return Ok(None);
    }
    let Some(context) = Context::new(ordinal)? else {
        return Ok(None);
    };
    check_cancelled(cancelled)?;
    let profile = std::env::var_os("DGX_POS2_PROFILE").is_some();
    if profile {
        eprintln!(
            "pos2_vulkan_resident selected capacity={} managed_bytes={} device_bytes={}",
            plan.capacity, plan.managed_bytes, plan.device_bytes
        );
    }
    let mut resident = Resident::new(context.clone(), params, plan.capacity)?;
    let mut remaining_work = limits.max_work;
    let started = Instant::now();
    let bindings = resident.bindings();
    for start in (0..INITIAL_ENTRIES).step_by(GENERATION_BATCH) {
        let count = GENERATION_BATCH.min(INITIAL_ENTRIES - start);
        charge(&mut remaining_work, count as u64, cancelled)?;
        resident.configure(
            0,
            start..start + count,
            0,
            start..plan.capacity,
            0,
            cancelled,
        )?;
        dispatch(&context, &resident.generate, &bindings, count)?;
        read_status(&context, &resident.counters, cancelled)?;
    }
    drop(bindings);
    std::mem::swap(&mut resident.inputs, &mut resident.scratch);
    if profile {
        eprintln!(
            "pos2_vulkan_resident generation seconds={:.3}",
            started.elapsed().as_secs_f64()
        );
    }
    let mut count = INITIAL_ENTRIES;
    let mut table_counts = [count, 0, 0, 0];
    for table in 1..=3u32 {
        let started = Instant::now();
        resident.sorter.sort(
            &mut resident.inputs,
            &mut resident.scratch,
            count,
            false,
            cancelled,
        )?;
        if profile {
            eprintln!(
                "pos2_vulkan_resident sort_{} seconds={:.3}",
                table - 1,
                started.elapsed().as_secs_f64()
            );
        }
        let bindings = resident.bindings();
        let started = Instant::now();
        if count != 0 {
            resident.configure(table, 0..count, count, 0..0, 0, cancelled)?;
            dispatch(&context, &resident.index, &bindings, count)?;
            read_status(&context, &resident.counters, cancelled)?;
        }
        if profile {
            eprintln!(
                "pos2_vulkan_resident index_{table} seconds={:.3}",
                started.elapsed().as_secs_f64()
            );
        }
        let started = Instant::now();
        let mut output_count = 0usize;
        for start in (0..count).step_by(MATCHING_BATCH) {
            let length = MATCHING_BATCH.min(count - start);
            charge(&mut remaining_work, length as u64 * 4, cancelled)?;
            let capacity = plan.capacity - output_count;
            resident.configure(
                table,
                start..start + length,
                count,
                output_count..plan.capacity,
                remaining_work,
                cancelled,
            )?;
            dispatch(&context, &resident.matching, &bindings, length)?;
            let status = read_status(&context, &resident.counters, cancelled)?;
            if status[0] as usize > capacity || u64::from(status[1]) > remaining_work {
                return Err(Error::other(
                    "Vulkan full-resident counters exceed dispatch budget",
                ));
            }
            charge(&mut remaining_work, u64::from(status[1]), cancelled)?;
            output_count += status[0] as usize;
        }
        drop(bindings);
        count = output_count;
        table_counts[table as usize] = count;
        std::mem::swap(&mut resident.inputs, &mut resident.scratch);
        if profile {
            eprintln!(
                "pos2_vulkan_resident matching_{table} seconds={:.3} entries={count}",
                started.elapsed().as_secs_f64()
            );
        }
    }
    let started = Instant::now();
    resident.sorter.sort(
        &mut resident.inputs,
        &mut resident.scratch,
        count,
        true,
        cancelled,
    )?;
    if profile {
        eprintln!(
            "pos2_vulkan_resident sort_3 seconds={:.3} work={}",
            started.elapsed().as_secs_f64(),
            limits.max_work - remaining_work
        );
    }
    let Resident { inputs, .. } = resident;
    context.check()?;
    check_cancelled(cancelled)?;
    Ok(Some(DevicePlot {
        gpu: context,
        entries: inputs,
        count,
        capacity: plan.capacity,
        params: params.clone(),
        table_counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> ProofParams {
        ProofParams::new([37; 32].into(), 28, 2, false).unwrap()
    }

    fn limits() -> PlotLimits {
        PlotLimits {
            memory_bytes: 12 << 30,
            max_entries: 310_000_000,
            max_work: 100_000_000_000,
        }
    }

    #[test]
    fn full_resident_preflight_and_work_are_bounded() {
        let cancelled = AtomicBool::new(false);
        let plan = memory_plan(limits().max_entries).unwrap();
        assert!(plan.capacity >= INITIAL_ENTRIES);
        assert!(plan.capacity <= SHARDS * SHARD_ENTRIES);
        assert!(plan.managed_bytes < 12 << 30);
        assert!(plan.device_bytes > plan.capacity as u64 * 32 + INDEX_BYTES);
        assert!(memory_plan(INITIAL_ENTRIES - 1).is_err());
        assert!(
            build_device(
                &params(),
                usize::MAX,
                PlotLimits {
                    memory_bytes: plan.managed_bytes - 1,
                    ..limits()
                },
                &cancelled
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            build_device(&params(), usize::MAX, limits(), &AtomicBool::new(true))
                .err()
                .unwrap()
                .kind(),
            ErrorKind::Interrupted
        );
        assert_eq!(
            build_device(
                &params(),
                usize::MAX,
                PlotLimits {
                    max_work: 0,
                    ..limits()
                },
                &cancelled
            )
            .err()
            .unwrap()
            .kind(),
            ErrorKind::InvalidInput
        );
        let unsupported = ProofParams::new([37; 32].into(), 28, 3, false).unwrap();
        assert!(
            build_device(&unsupported, usize::MAX, limits(), &cancelled)
                .unwrap()
                .is_none()
        );
        let mut remaining = 17;
        charge(&mut remaining, 17, &cancelled).unwrap();
        assert!(charge(&mut remaining, 1, &cancelled).is_err());
        assert_eq!(remaining, 0);
    }

    #[test]
    fn full_resident_shared_shader_validates_without_gpu() {
        let source = shader_source();
        assert!(!source.contains("    output[position] = value;"));
        let module = naga::front::wgsl::parse_str(&source)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(&source)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("full-resident shader must use portable WGSL");
    }

    #[test]
    #[ignore = "requires an explicit DGX_VULKAN_TEST_DEVICE and full-resident Vulkan support"]
    fn full_resident_gpu_generation_download_and_limits_match_cpu() {
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .expect("set selected Vulkan device")
            .parse::<usize>()
            .expect("device ordinal must be an integer");
        let gpu = Context::new(ordinal)
            .unwrap()
            .expect("selected adapter must support full-resident plotting");
        let params = params();
        let cancelled = AtomicBool::new(false);
        let mut resident = Resident::new(gpu.clone(), &params, 1024).unwrap();
        let start = INITIAL_ENTRIES - 257;
        resident
            .configure(0, start..INITIAL_ENTRIES, 0, 13..1024, 0, &cancelled)
            .unwrap();
        let bindings = resident.bindings();
        dispatch(&gpu, &resident.generate, &bindings, 257).unwrap();
        assert_eq!(
            read_status(&gpu, &resident.counters, &cancelled).unwrap(),
            [0; 4]
        );
        let actual = gpu
            .read(&resident.scratch[0], 13 * 16, 257 * 16, &cancelled)
            .unwrap();
        for (offset, bytes) in actual.as_chunks::<16>().0.iter().enumerate() {
            let actual = bytemuck::pod_read_unaligned::<Entry>(bytes);
            let expected = device::generate(config(&params), (start + offset) as u32);
            assert_eq!(
                (actual.meta, actual.info, actual.x_bits),
                (expected.meta, expected.info, expected.x_bits)
            );
        }
        resident
            .configure(0, start..INITIAL_ENTRIES, 0, 0..256, 0, &cancelled)
            .unwrap();
        dispatch(&gpu, &resident.generate, &bindings, 257).unwrap();
        assert!(read_status(&gpu, &resident.counters, &cancelled).is_err());
        for testnet in [false, true] {
            let fixture_params = ProofParams::new([37; 32].into(), 28, 2, testnet).unwrap();
            let configuration = config(&fixture_params);
            resident.parameters = parameters(&fixture_params);
            for table in 1..=3 {
                let left = Entry {
                    meta: if table == 1 {
                        0x0123_4567
                    } else {
                        0x007a_bcde_f012_3456
                    },
                    info: ((table - 1) << 26) | 0x0001_2345,
                    x_bits: if table == 3 { 0x0123_4567 } else { 0 },
                };
                let record = |entry: Entry| device::Record {
                    meta: entry.meta,
                    info: entry.info,
                    x_bits: entry.x_bits,
                    ..device::Record::default()
                };
                let mut entries = vec![left];
                let mut expected = Vec::new();
                for key in 0..4 {
                    let wanted = device::target(configuration, table, record(left), key);
                    let (right, result) = (0..4096)
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
                            let result =
                                device::pair(configuration, table, record(left), record(right));
                            (result.valid != 0).then_some((right, result))
                        })
                        .expect("bounded fixture search must find a passing pair");
                    entries.push(right);
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
                entries.sort_unstable_by_key(|entry| entry.info);
                let left_index = entries
                    .iter()
                    .position(|entry| entry.info == left.info)
                    .unwrap();
                gpu.queue
                    .write_buffer(&resident.inputs[0], 0, bytemuck::cast_slice(&entries));
                for (position, entry) in entries.iter().enumerate() {
                    if position != left_index {
                        gpu.queue.write_buffer(
                            &resident.buckets,
                            u64::from(entry.info) * 4,
                            bytemuck::cast_slice(&[position as u32, position as u32 + 1]),
                        );
                    }
                }
                for (pair_budget, output_capacity, succeeds) in
                    [(0, 4, false), (4, 0, false), (4, 4, true)]
                {
                    resident
                        .configure(
                            table,
                            left_index..left_index + 1,
                            entries.len(),
                            13..13 + output_capacity,
                            pair_budget,
                            &cancelled,
                        )
                        .unwrap();
                    dispatch(&gpu, &resident.matching, &bindings, 1).unwrap();
                    let result = read_status(&gpu, &resident.counters, &cancelled);
                    if !succeeds {
                        assert!(result.is_err());
                        continue;
                    }
                    assert_eq!(result.unwrap(), [4, 4, 0, 0]);
                    let bytes = gpu
                        .read(&resident.scratch[0], 13 * 16, 4 * 16, &cancelled)
                        .unwrap();
                    let mut actual: Vec<_> = bytes
                        .as_chunks::<16>()
                        .0
                        .iter()
                        .map(|bytes| {
                            let entry = bytemuck::pod_read_unaligned::<Entry>(bytes);
                            (entry.meta, entry.info, entry.x_bits)
                        })
                        .collect();
                    actual.sort_unstable();
                    expected.sort_unstable();
                    assert_eq!(actual, expected, "table {table}, testnet {testnet}");
                }
            }
        }
        drop(bindings);
        drop(resident);
        let values: Vec<u64> = (0..257)
            .map(|position| ((position as u64) << 40) | (position as u64 * 17))
            .collect();
        let entries: Vec<Entry> = values
            .iter()
            .map(|value| Entry {
                meta: *value,
                info: 0,
                x_bits: 0,
            })
            .collect();
        let buffers = allocate_entries(&gpu, entries.len(), "PoS2 final fragment fixture").unwrap();
        gpu.queue
            .write_buffer(&buffers[0], 0, bytemuck::cast_slice(&entries));
        let plot = DevicePlot {
            gpu: gpu.clone(),
            entries: buffers,
            count: values.len(),
            capacity: values.len(),
            params: params.clone(),
            table_counts: [INITIAL_ENTRIES, 0, 0, values.len()],
        };
        assert_eq!(plot.download(&cancelled).unwrap().fragments(), values);
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        assert!(gpu.read(&buffer, 1, 4, &cancelled).is_err());
        assert!(gpu.read(&buffer, 0, 20, &cancelled).is_err());
        assert_eq!(
            gpu.read(&buffer, 0, 4, &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            ErrorKind::Interrupted
        );
    }
}
