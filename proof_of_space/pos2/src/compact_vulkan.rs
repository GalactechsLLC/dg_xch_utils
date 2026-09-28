use super::resident::{self, Backend, BatchStatus, INDEX_BYTES, OUTPUT_ENTRIES, TRANSFER_BYTES};
use super::{CompactPlot, Entry};
use crate::compute::{check_cancelled, config};
use crate::device;
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const SHARD_ENTRIES: usize = 1 << 26;
const SHARDS: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(30);

struct Resident {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    generate: wgpu::ComputePipeline,
    index: wgpu::ComputePipeline,
    matching: wgpu::ComputePipeline,
    configuration: wgpu::Buffer,
    inputs: Vec<wgpu::Buffer>,
    output: wgpu::Buffer,
    buckets: wgpu::Buffer,
    counters: wgpu::Buffer,
    aes_table: wgpu::Buffer,
    readback: wgpu::Buffer,
    bindings: Option<wgpu::BindGroup>,
    failure: Arc<Mutex<Option<String>>>,
    parameters: [u32; 32],
}

impl Resident {
    fn new(params: &ProofParams, ordinal: usize) -> Result<Option<Self>, Error> {
        let instance =
            crate::vulkan::instance().ok_or_else(|| Error::other("Vulkan backend unavailable"))?;
        let adapter = crate::vulkan::hardware_adapters(&instance)
            .into_iter()
            .nth(ordinal)
            .ok_or_else(|| Error::other("requested Vulkan plotting adapter unavailable"))?;
        let supported = adapter.limits();
        if supported.max_storage_buffers_per_shader_stage < 9
            || supported.max_storage_buffer_binding_size < INDEX_BYTES
            || supported.max_buffer_size < INDEX_BYTES
        {
            return Ok(None);
        }
        let required_limits = wgpu::Limits {
            max_storage_buffers_per_shader_stage: 9,
            max_storage_buffer_binding_size: INDEX_BYTES,
            max_buffer_size: INDEX_BYTES,
            ..wgpu::Limits::downlevel_defaults()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("PoS2 resident k28 plotting"),
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
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let entries: Vec<_> = (0..10)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 0 {
                        wgpu::BufferBindingType::Uniform
                    } else {
                        wgpu::BufferBindingType::Storage {
                            read_only: matches!(binding, 1..=5 | 9),
                        }
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("PoS2 resident bindings"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("PoS2 resident pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("PoS2 resident k28 shader"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("vulkan_aes.wgsl"),
                    "\n",
                    include_str!("vulkan_compact.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let generate = pipeline("generate");
        let index = pipeline("build_index");
        let matching = pipeline("match_table");
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::other(format!("Vulkan resident shader: {error}")));
        }
        let buffer = |label, size, usage| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let configuration = buffer(
            "PoS2 resident parameters",
            128,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let output = buffer(
            "PoS2 resident bounded output",
            TRANSFER_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let buckets = buffer(
            "PoS2 resident dense index",
            INDEX_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let counters = buffer(
            "PoS2 resident counters",
            16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        );
        let aes_table = buffer(
            "PoS2 resident AES table",
            1024,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let readback = buffer(
            "PoS2 resident readback",
            TRANSFER_BYTES,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        queue.write_buffer(&aes_table, 0, bytemuck::cast_slice(&device::AES_TABLE));
        let configuration_values = config(params);
        let mut parameters = [0; 32];
        for (destination, bytes) in parameters[..8]
            .iter_mut()
            .zip(configuration_values.plot_id.as_chunks::<4>().0)
        {
            *destination = u32::from_le_bytes(*bytes);
        }
        for (destination, keys) in parameters[8..24]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(device::fragment_round_keys(configuration_values))
        {
            *destination = keys;
        }
        parameters[25] = u32::from(params.is_testnet());
        parameters[29] = OUTPUT_ENTRIES as u32;
        let mut result = Self {
            device,
            queue,
            layout,
            generate,
            index,
            matching,
            configuration,
            inputs: Vec::new(),
            output,
            buckets,
            counters,
            aes_table,
            readback,
            bindings: None,
            failure,
            parameters,
        };
        result.install_empty_inputs();
        result.check_error()?;
        Ok(Some(result))
    }

    fn check_error(&self) -> Result<(), Error> {
        let failure = self
            .failure
            .lock()
            .map_err(|_| Error::other("Vulkan error lock poisoned"))?;
        match failure.as_ref() {
            Some(error) => Err(Error::other(format!("Vulkan resident plotting: {error}"))),
            None => Ok(()),
        }
    }

    fn input_buffer(&self, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 resident input shard"),
            size: size.max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn install_empty_inputs(&mut self) {
        self.inputs = (0..SHARDS).map(|_| self.input_buffer(16)).collect();
        self.bind();
    }

    fn bind(&mut self) {
        let buffers: Vec<_> = std::iter::once(&self.configuration)
            .chain(&self.inputs)
            .chain([&self.output, &self.buckets, &self.counters, &self.aes_table])
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        self.bindings = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PoS2 resident table bindings"),
            layout: &self.layout,
            entries: &buffers,
        }));
    }

    fn release_inputs(&mut self) {
        self.bindings = None;
        for buffer in self.inputs.drain(..) {
            buffer.destroy();
        }
    }

    fn wait(&self, submission: wgpu::SubmissionIndex, cancelled: &AtomicBool) -> Result<(), Error> {
        let started = Instant::now();
        loop {
            check_cancelled(cancelled)?;
            self.check_error()?;
            let remaining = TIMEOUT.checked_sub(started.elapsed()).ok_or_else(|| {
                Error::new(ErrorKind::TimedOut, "Vulkan resident operation timed out")
            })?;
            match self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission.clone()),
                timeout: Some(remaining.min(Duration::from_millis(100))),
            }) {
                Ok(_) => return self.check_error(),
                Err(wgpu::PollError::Timeout) => {}
                Err(error) => return Err(Error::other(format!("Vulkan polling: {error}"))),
            }
        }
    }

    fn upload(&mut self, entries: &[Entry], cancelled: &AtomicBool) -> Result<(), Error> {
        self.release_inputs();
        for shard in entries.chunks(SHARD_ENTRIES) {
            let buffer = self.input_buffer(std::mem::size_of_val(shard) as u64);
            self.check_error()?;
            for (window, values) in shard.chunks(OUTPUT_ENTRIES).enumerate() {
                check_cancelled(cancelled)?;
                self.queue.write_buffer(
                    &buffer,
                    window as u64 * TRANSFER_BYTES,
                    bytemuck::cast_slice(values),
                );
                self.wait(self.queue.submit([]), cancelled)?;
            }
            self.inputs.push(buffer);
        }
        while self.inputs.len() < SHARDS {
            self.inputs.push(self.input_buffer(16));
        }
        self.parameters[28] = u32::try_from(entries.len())
            .map_err(|_| Error::other("Vulkan input index overflow"))?;
        self.bind();
        self.check_error()
    }

    fn configure(&mut self, table: u32, start: usize, count: usize, pair_budget: u64) {
        self.parameters[24] = table;
        self.parameters[26] = start as u32;
        self.parameters[27] = count as u32;
        self.parameters[30] = pair_budget.min(u64::from(u32::MAX)) as u32;
        self.queue.write_buffer(
            &self.configuration,
            0,
            bytemuck::cast_slice(&self.parameters),
        );
        self.queue.write_buffer(&self.counters, 0, &[0; 16]);
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PoS2 resident command"),
            })
    }

    fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        count: usize,
    ) -> Result<(), Error> {
        let bindings = self
            .bindings
            .as_ref()
            .ok_or_else(|| Error::other("Vulkan table is not bound"))?;
        let groups = (count as u32).div_ceil(64);
        let height = groups.div_ceil(65_535).max(1);
        let width = groups.div_ceil(height);
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("PoS2 resident compute"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bindings, &[]);
        pass.dispatch_workgroups(width, height, 1);
        Ok(())
    }

    fn read<ResultValue>(
        &self,
        encoder: wgpu::CommandEncoder,
        size: u64,
        cancelled: &AtomicBool,
        consume: impl FnOnce(&[u8]) -> Result<ResultValue, Error>,
    ) -> Result<ResultValue, Error> {
        let submission = self.queue.submit([encoder.finish()]);
        let slice = self.readback.slice(..size);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        let result = (|| {
            self.wait(submission, cancelled)?;
            receiver
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| Error::other("Vulkan readback callback unavailable"))?
                .map_err(|error| Error::other(format!("Vulkan readback: {error}")))?;
            let mapped = slice
                .get_mapped_range()
                .map_err(|error| Error::other(format!("Vulkan map: {error}")))?;
            let result = consume(&mapped);
            drop(mapped);
            self.check_error()?;
            check_cancelled(cancelled)?;
            result
        })();
        self.readback.unmap();
        result
    }

    fn read_status(
        &self,
        mut encoder: wgpu::CommandEncoder,
        cancelled: &AtomicBool,
    ) -> Result<[u32; 4], Error> {
        encoder.copy_buffer_to_buffer(&self.counters, 0, &self.readback, 0, 16);
        let status = self.read(encoder, 16, cancelled, |bytes| {
            Ok(std::array::from_fn(|index| {
                let offset = index * 4;
                u32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ])
            }))
        })?;
        if status[2] != 0 {
            return Err(Error::other(format!(
                "Vulkan resident table rejected: flags={:#x}, outputs={}, pairs={}",
                status[2], status[0], status[1]
            )));
        }
        Ok(status)
    }

    fn read_entries(
        &self,
        mut encoder: wgpu::CommandEncoder,
        count: usize,
        destination: &mut Vec<Entry>,
        capacity: usize,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        if count == 0 {
            return self.wait(self.queue.submit([encoder.finish()]), cancelled);
        }
        if count > OUTPUT_ENTRIES || count > capacity.saturating_sub(destination.len()) {
            return Err(Error::other("Vulkan compact table exceeds entry budget"));
        }
        let size = (count * size_of::<Entry>()) as u64;
        encoder.copy_buffer_to_buffer(&self.output, 0, &self.readback, 0, size);
        self.read(encoder, size, cancelled, |bytes| {
            let entries = bytemuck::try_cast_slice(bytes)
                .map_err(|_| Error::other("Vulkan entry readback alignment invalid"))?;
            destination.extend_from_slice(entries);
            Ok(())
        })
    }
}

impl Backend for Resident {
    fn generate(
        &mut self,
        start: usize,
        count: usize,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.configure(0, start, count, 0);
        let mut encoder = self.encoder();
        self.dispatch(&mut encoder, &self.generate, count)?;
        self.read_status(encoder, cancelled)?;
        Ok(())
    }

    fn upload_index(
        &mut self,
        table: u32,
        entries: &[Entry],
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        self.upload(entries, cancelled)?;
        self.configure(table, 0, entries.len(), 0);
        let mut encoder = self.encoder();
        self.dispatch(&mut encoder, &self.index, entries.len())?;
        self.read_status(encoder, cancelled)?;
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
        self.configure(table, start, count, pair_budget);
        let mut encoder = self.encoder();
        self.dispatch(&mut encoder, &self.matching, count)?;
        let status = self.read_status(encoder, cancelled)?;
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
        Resident::read_entries(
            self,
            self.encoder(),
            count,
            destination,
            capacity,
            cancelled,
        )
    }

    fn release_inputs(&mut self) -> Result<(), Error> {
        Resident::release_inputs(self);
        Ok(())
    }
}

pub(crate) fn build(
    params: &ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    ordinal: usize,
) -> Result<Option<CompactPlot>, Error> {
    resident::build(params, limits, cancelled, || Resident::new(params, ordinal))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute::SCRATCH_BYTES;

    fn params() -> ProofParams {
        ProofParams::new([37; 32].into(), 28, 2, false).unwrap()
    }

    fn limits() -> PlotLimits {
        PlotLimits {
            memory_bytes: 12 * 1024 * 1024 * 1024,
            max_entries: 310_000_000,
            max_work: 100_000_000_000,
        }
    }

    fn required_memory(limits: PlotLimits) -> u64 {
        CompactPlot::memory_required(28, limits.max_entries).unwrap()
            + INDEX_BYTES
            + 3 * TRANSFER_BYTES
            + 1024 * 1024
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn k28_resident_cancellation_precedes_device_opening() {
        let error = build(&params(), limits(), &AtomicBool::new(true), usize::MAX)
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn k28_resident_rejects_insufficient_entries_before_device_opening() {
        for max_entries in [0, (1 << 28) - 1] {
            let error = build(
                &params(),
                PlotLimits {
                    max_entries,
                    ..limits()
                },
                &AtomicBool::new(false),
                usize::MAX,
            )
            .err()
            .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn k28_resident_requires_combined_memory_before_device_opening() {
        let limits = limits();
        let base = CompactPlot::memory_required(28, limits.max_entries).unwrap();
        for memory_bytes in [0, base, required_memory(limits) - 1] {
            let result = build(
                &params(),
                PlotLimits {
                    memory_bytes,
                    ..limits
                },
                &AtomicBool::new(false),
                usize::MAX,
            )
            .unwrap();
            assert!(result.is_none());
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn k28_resident_exact_memory_boundary_checks_work_before_device_opening() {
        let params = params();
        let limits = limits();
        for max_work in [0, CompactPlot::minimum_work(&params) - 1] {
            let error = build(
                &params,
                PlotLimits {
                    memory_bytes: required_memory(limits),
                    max_work,
                    ..limits
                },
                &AtomicBool::new(false),
                usize::MAX,
            )
            .err()
            .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn resident_unsupported_parameters_do_not_open_device() {
        for (plot_size, strength) in [(18, 2), (26, 2), (28, 3), (30, 2), (32, 2)] {
            let params = ProofParams::new([37; 32].into(), plot_size, strength, false).unwrap();
            let result = build(&params, limits(), &AtomicBool::new(false), usize::MAX).unwrap();
            assert!(result.is_none());
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    fn k28_resident_wire_layout_and_transfer_bounds_match_shader() {
        let entry = Entry {
            meta: 0x8877_6655_4433_2211,
            info: 0xaabb_ccdd,
            x_bits: 0x0123_4567,
        };
        let words: [u32; 4] = bytemuck::cast(entry);
        assert_eq!(words, [0x4433_2211, 0x8877_6655, 0xaabb_ccdd, 0x0123_4567]);
        assert_eq!(size_of::<Entry>(), 16);
        assert_eq!(SHARD_ENTRIES, 1 << 26);
        assert_eq!(TRANSFER_BYTES, 64 * 1024 * 1024);
        assert_eq!(OUTPUT_ENTRIES * size_of::<Entry>(), TRANSFER_BYTES as usize);
        assert_eq!(SHARD_ENTRIES % OUTPUT_ENTRIES, 0);
        assert_eq!(INDEX_BYTES, ((1u64 << 28) + 1) * size_of::<u32>() as u64);
        let maximum_capacity = (CompactPlot::memory_required(28, usize::MAX).unwrap()
            - SCRATCH_BYTES)
            / (2 * size_of::<Entry>() as u64);
        assert!(maximum_capacity <= (SHARDS * SHARD_ENTRIES) as u64);
        assert!(maximum_capacity <= u64::from(u32::MAX));
    }

    #[test]
    #[cfg(target_endian = "little")]
    #[ignore = "requires an explicit DGX_VULKAN_TEST_DEVICE and a hardware GPU with resident plotting support"]
    fn k28_resident_gpu_generation_matching_and_limits_match_cpu() {
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .expect("set DGX_VULKAN_TEST_DEVICE to the selected hardware Vulkan adapter")
            .parse::<usize>()
            .expect("DGX_VULKAN_TEST_DEVICE must be an adapter ordinal");
        let params = params();
        let configuration = config(&params);
        let cancelled = AtomicBool::new(false);
        let mut gpu = Resident::new(&params, ordinal)
            .unwrap()
            .expect("selected Vulkan adapter must support resident k28 plotting");
        let count = 257;
        let start = (1usize << 28) - count;
        gpu.configure(0, start, count, 0);
        let mut encoder = gpu.encoder();
        gpu.dispatch(&mut encoder, &gpu.generate, count).unwrap();
        assert_eq!(gpu.read_status(encoder, &cancelled).unwrap(), [0; 4]);
        let mut generated = Vec::with_capacity(count);
        gpu.read_entries(gpu.encoder(), count, &mut generated, count, &cancelled)
            .unwrap();
        for (offset, actual) in generated.iter().enumerate() {
            let expected = device::generate(configuration, (start + offset) as u32);
            assert_eq!(
                (actual.meta, actual.info, actual.x_bits),
                (expected.meta, expected.info, expected.x_bits)
            );
        }

        for table in 1..=3u32 {
            let left = Entry {
                meta: if table == 1 {
                    0x0123_4567
                } else {
                    0x007a_bcde_f012_3456
                },
                info: ((table - 1) << 26) | 0x0001_2345,
                x_bits: if table == 3 { 0x0123_4567 } else { 0 },
            };
            let mut entries = vec![left];
            let mut expected = Vec::with_capacity(4);
            for key in 0..4u32 {
                let wanted = device::target(configuration, table, left.record(), key);
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
                        let result =
                            device::pair(configuration, table, left.record(), right.record());
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
            gpu.upload(&entries, &cancelled).unwrap();
            for (position, entry) in entries.iter().enumerate() {
                if position != left_index {
                    let bounds = [position as u32, position as u32 + 1];
                    gpu.queue.write_buffer(
                        &gpu.buckets,
                        u64::from(entry.info) * size_of::<u32>() as u64,
                        bytemuck::cast_slice(&bounds),
                    );
                }
            }
            for (pair_budget, output_capacity, expected_status) in [
                (0, 4, [0, 0, 2, 0]),
                (4, 0, [0, 4, 1, 0]),
                (4, 4, [4, 4, 0, 0]),
            ] {
                gpu.parameters[29] = output_capacity;
                gpu.configure(table, left_index, 1, pair_budget);
                let mut encoder = gpu.encoder();
                gpu.dispatch(&mut encoder, &gpu.matching, 1).unwrap();
                encoder.copy_buffer_to_buffer(&gpu.counters, 0, &gpu.readback, 0, 16);
                let status = gpu
                    .read(encoder, 16, &cancelled, |bytes| {
                        Ok(bytemuck::pod_read_unaligned::<[u32; 4]>(bytes))
                    })
                    .unwrap();
                assert_eq!(status, expected_status, "table {table}");
                let checked = gpu.read_status(gpu.encoder(), &cancelled);
                if status[2] != 0 {
                    assert!(checked.is_err(), "table {table} must reject shader errors");
                    continue;
                }
                assert_eq!(checked.unwrap(), expected_status);
                let mut output = Vec::with_capacity(4);
                gpu.read_entries(
                    gpu.encoder(),
                    status[0] as usize,
                    &mut output,
                    4,
                    &cancelled,
                )
                .unwrap();
                let mut actual: Vec<_> = output
                    .iter()
                    .map(|entry| (entry.meta, entry.info, entry.x_bits))
                    .collect();
                actual.sort_unstable();
                expected.sort_unstable();
                assert_eq!(actual, expected, "table {table}");
            }
            gpu.release_inputs();
        }
    }
}
