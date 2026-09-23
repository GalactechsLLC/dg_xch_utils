use crate::compute::check_cancelled;
use crate::vulkan_full::Context;
use std::io::{Error, ErrorKind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const ITEMS: usize = 1024;
const BINS: usize = 256;
const PREFIX_ITEMS: usize = 1024;
const SHARDS: usize = 5;
const SHARD_ENTRIES: usize = 1 << 26;
const PASSES: usize = 7;
const CONFIGURATION_BYTES: u64 = 32;

fn dimensions(capacity: usize) -> Result<(usize, usize), Error> {
    if capacity > SHARDS * SHARD_ENTRIES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Vulkan radix capacity exceeds the five-shard limit",
        ));
    }
    let blocks = capacity.div_ceil(ITEMS);
    Ok((blocks, blocks.div_ceil(PREFIX_ITEMS)))
}

pub(crate) fn scratch_bytes(capacity: usize) -> Result<u64, Error> {
    let (blocks, chunks) = dimensions(capacity)?;
    let words = blocks
        .max(1)
        .checked_add(chunks.max(1))
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_mul(BINS))
        .and_then(|count| count.checked_add(1))
        .ok_or_else(|| Error::other("Vulkan radix metadata size overflow"))?;
    u64::try_from(words)
        .ok()
        .and_then(|words| words.checked_mul(4))
        .and_then(|bytes| bytes.checked_add(PASSES as u64 * CONFIGURATION_BYTES))
        .ok_or_else(|| Error::other("Vulkan radix metadata size overflow"))
}

pub(crate) struct Sorter {
    gpu: Arc<Context>,
    layout: wgpu::BindGroupLayout,
    histogram_pipeline: wgpu::ComputePipeline,
    chunks_pipeline: wgpu::ComputePipeline,
    totals_pipeline: wgpu::ComputePipeline,
    bins_pipeline: wgpu::ComputePipeline,
    scatter_pipeline: wgpu::ComputePipeline,
    configurations: Vec<wgpu::Buffer>,
    histogram: wgpu::Buffer,
    sums: wgpu::Buffer,
    bins: wgpu::Buffer,
    errors: wgpu::Buffer,
    capacity: usize,
}

impl Sorter {
    pub(crate) fn new(gpu: Arc<Context>, capacity: usize) -> Result<Self, Error> {
        scratch_bytes(capacity)?;
        let limits = gpu.device.limits();
        if !gpu.device.features().contains(wgpu::Features::SUBGROUP)
            || limits.max_compute_workgroup_storage_size < 32 * 1024
            || limits.max_compute_invocations_per_workgroup < 256
            || limits.max_compute_workgroup_size_x < 256
            || limits.max_storage_buffers_per_shader_stage < 14
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Vulkan radix requires subgroups, 32 KiB shared memory, 256 threads and 14 storage bindings",
            ));
        }
        gpu.check()?;
        let (blocks, chunks) = dimensions(capacity)?;
        let histogram_bytes = (blocks.max(1) * BINS * size_of::<u32>()) as u64;
        if histogram_bytes > limits.max_storage_buffer_binding_size
            || histogram_bytes > limits.max_buffer_size
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Vulkan radix histogram exceeds device limits",
            ));
        }
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let entries: Vec<_> = (0..=14)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 0 {
                        wgpu::BufferBindingType::Uniform
                    } else {
                        wgpu::BufferBindingType::Storage {
                            read_only: binding <= SHARDS as u32,
                        }
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("PoS2 full-resident radix bindings"),
                entries: &entries,
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PoS2 full-resident radix layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("PoS2 subgroup radix shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("vulkan_radix.wgsl").into()),
            });
        let pipeline = |entry_point| {
            gpu.device
                .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry_point),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: Some(entry_point),
                    compilation_options: Default::default(),
                    cache: None,
                })
        };
        let histogram_pipeline = pipeline("radix_histogram");
        let chunks_pipeline = pipeline("radix_prefix_chunks");
        let totals_pipeline = pipeline("radix_prefix_totals");
        let bins_pipeline = pipeline("radix_prefix_bins");
        let scatter_pipeline = pipeline("radix_scatter");
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::other(format!("Vulkan radix shader: {error}")));
        }
        let buffer = |label, size, usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let configurations = (0..PASSES)
            .map(|_| {
                buffer(
                    "PoS2 radix pass configuration",
                    CONFIGURATION_BYTES,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                )
            })
            .collect();
        let histogram = buffer(
            "PoS2 radix block histogram",
            histogram_bytes,
            wgpu::BufferUsages::STORAGE,
        );
        let sums = buffer(
            "PoS2 radix prefix chunks",
            (chunks.max(1) * BINS * 4) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let bins = buffer(
            "PoS2 radix bucket prefixes",
            (BINS * 4) as u64,
            wgpu::BufferUsages::STORAGE,
        );
        let errors = buffer(
            "PoS2 radix errors",
            4,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
        );
        gpu.check()?;
        Ok(Self {
            gpu,
            layout,
            histogram_pipeline,
            chunks_pipeline,
            totals_pipeline,
            bins_pipeline,
            scatter_pipeline,
            configurations,
            histogram,
            sums,
            bins,
            errors,
            capacity,
        })
    }

    fn validate_shards(&self, shards: &[wgpu::Buffer], count: usize) -> Result<(), Error> {
        if shards.len() != SHARDS {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Vulkan radix requires five entry shards",
            ));
        }
        for (index, buffer) in shards.iter().enumerate() {
            let entries = count
                .saturating_sub(index * SHARD_ENTRIES)
                .min(SHARD_ENTRIES);
            if buffer.size() < (entries.max(1) * size_of::<[u32; 4]>()) as u64
                || !buffer.usage().contains(wgpu::BufferUsages::STORAGE)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Vulkan radix entry shard is too small or not a storage buffer",
                ));
            }
        }
        Ok(())
    }

    fn bindings(
        &self,
        configuration: &wgpu::Buffer,
        input: &[wgpu::Buffer],
        output: &[wgpu::Buffer],
    ) -> wgpu::BindGroup {
        let entries: Vec<_> = std::iter::once(configuration)
            .chain(input)
            .chain(output)
            .chain([&self.histogram, &self.sums, &self.bins, &self.errors])
            .enumerate()
            .map(|(binding, buffer)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PoS2 radix pass bindings"),
                layout: &self.layout,
                entries: &entries,
            })
    }

    fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        bindings: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("PoS2 full-resident radix pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bindings, &[]);
        pass.dispatch_workgroups(width, height, 1);
    }

    pub(crate) fn sort(
        &mut self,
        input: &mut Vec<wgpu::Buffer>,
        scratch: &mut Vec<wgpu::Buffer>,
        count: usize,
        final_table: bool,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        check_cancelled(cancelled)?;
        if count > self.capacity {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Vulkan radix input exceeds capacity",
            ));
        }
        self.validate_shards(input, count)?;
        self.validate_shards(scratch, count)?;
        if count == 0 {
            return Ok(());
        }
        self.gpu.check()?;
        self.gpu.queue.write_buffer(&self.errors, 0, &[0; 4]);
        let (blocks, chunks) = dimensions(count)?;
        let height = (blocks as u32).div_ceil(65_535).max(1);
        let width = (blocks as u32).div_ceil(height);
        let passes = if final_table { 7 } else { 4 };
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PoS2 GPU-only radix sort"),
            });
        for pass in 0..passes {
            check_cancelled(cancelled)?;
            let configuration = self
                .configurations
                .get(pass)
                .ok_or_else(|| Error::other("Vulkan radix pass configuration is missing"))?;
            let values = [
                count as u32,
                blocks as u32,
                chunks as u32,
                pass as u32 * 8,
                u32::from(final_table),
                0,
                0,
                0,
            ];
            self.gpu
                .queue
                .write_buffer(configuration, 0, bytemuck::cast_slice(&values));
            let bindings = self.bindings(configuration, input, scratch);
            self.dispatch(
                &mut encoder,
                &self.histogram_pipeline,
                &bindings,
                width,
                height,
            );
            self.dispatch(
                &mut encoder,
                &self.chunks_pipeline,
                &bindings,
                chunks as u32,
                BINS as u32,
            );
            self.dispatch(
                &mut encoder,
                &self.totals_pipeline,
                &bindings,
                BINS as u32,
                1,
            );
            self.dispatch(&mut encoder, &self.bins_pipeline, &bindings, 1, 1);
            self.dispatch(
                &mut encoder,
                &self.scatter_pipeline,
                &bindings,
                width,
                height,
            );
            std::mem::swap(input, scratch);
        }
        let submission = self.gpu.queue.submit([encoder.finish()]);
        self.gpu.wait(submission, cancelled)?;
        let bytes = self.gpu.read(&self.errors, 0, 4, cancelled)?;
        let flags = bytes
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| Error::other("Vulkan radix status is truncated"))?;
        if flags != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Vulkan radix rejected data: flags={flags:#x}"),
            ));
        }
        check_cancelled(cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vulkan_full::allocate_entries;
    use std::sync::atomic::Ordering;

    #[test]
    fn vulkan_radix_metadata_budget_is_bounded() {
        assert_eq!(scratch_bytes(0).unwrap(), 3300);
        assert_eq!(scratch_bytes(1).unwrap(), scratch_bytes(1024).unwrap());
        assert_eq!(scratch_bytes(1025).unwrap(), 4324);
        assert!(scratch_bytes(310_000_000).unwrap() < 304 * 1024 * 1024);
        assert!(scratch_bytes(SHARDS * SHARD_ENTRIES + 1).is_err());
        assert!(scratch_bytes(usize::MAX).is_err());
    }

    #[test]
    #[ignore = "requires an explicit DGX_VULKAN_TEST_DEVICE with subgroup plotting support"]
    fn vulkan_radix_prefix_carry_crosses_256_chunks_without_large_tables() {
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .expect("set DGX_VULKAN_TEST_DEVICE to the selected hardware Vulkan adapter")
            .parse::<usize>()
            .expect("DGX_VULKAN_TEST_DEVICE must be an adapter ordinal");
        let gpu = Context::new(ordinal)
            .unwrap()
            .expect("selected adapter must support subgroup plotting");
        let mut sorter = Sorter::new(gpu.clone(), 1).unwrap();
        let cancelled = AtomicBool::new(false);
        let input = allocate_entries(&gpu, 1, "radix metadata test input").unwrap();
        let scratch = allocate_entries(&gpu, 1, "radix metadata test scratch").unwrap();
        sorter.bins = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("radix metadata test bucket prefixes"),
            size: (BINS * size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        for chunks in [256, 257, 289] {
            let values: Vec<u32> = (0..BINS)
                .flat_map(|bucket| {
                    (0..chunks).map(move |chunk| {
                        if bucket == 17 || (bucket + chunk) % 31 == 0 {
                            0
                        } else {
                            ((bucket * 37 + chunk * 101) % 8192) as u32
                        }
                    })
                })
                .collect();
            let mut expected_sums = vec![0u32; values.len()];
            let mut expected_bins = [0u32; BINS];
            let mut total = 0u32;
            for (bucket, expected_bin) in expected_bins.iter_mut().enumerate() {
                *expected_bin = total;
                let mut prefix = 0u32;
                for chunk in 0..chunks {
                    let position = bucket * chunks + chunk;
                    expected_sums[position] = prefix;
                    prefix += values[position];
                }
                total += prefix;
            }
            let bytes = (values.len() * size_of::<u32>()) as u64;
            sorter.sums = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("radix bounded metadata test chunk counts"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            gpu.queue
                .write_buffer(&sorter.sums, 0, bytemuck::cast_slice(&values));
            let configuration = &sorter.configurations[0];
            gpu.queue.write_buffer(
                configuration,
                0,
                bytemuck::cast_slice(&[
                    total,
                    (chunks * PREFIX_ITEMS) as u32,
                    chunks as u32,
                    0,
                    0,
                    0,
                    0,
                    0,
                ]),
            );
            let bindings = sorter.bindings(configuration, &input, &scratch);
            let mut encoder = gpu.encoder("radix bounded metadata carry test");
            sorter.dispatch(
                &mut encoder,
                &sorter.totals_pipeline,
                &bindings,
                BINS as u32,
                1,
            );
            sorter.dispatch(&mut encoder, &sorter.bins_pipeline, &bindings, 1, 1);
            gpu.wait(gpu.queue.submit([encoder.finish()]), &cancelled)
                .unwrap();
            let actual_sums = gpu.read(&sorter.sums, 0, bytes, &cancelled).unwrap();
            assert_eq!(
                bytemuck::cast_slice::<u8, u32>(&actual_sums),
                expected_sums.as_slice(),
                "chunks={chunks}"
            );
            let actual_bins = gpu
                .read(&sorter.bins, 0, (BINS * 4) as u64, &cancelled)
                .unwrap();
            assert_eq!(
                bytemuck::cast_slice::<u8, u32>(&actual_bins),
                expected_bins.as_slice(),
                "chunks={chunks}"
            );
        }
    }

    #[test]
    #[ignore = "requires an explicit DGX_VULKAN_TEST_DEVICE with subgroup plotting support"]
    fn vulkan_radix_matches_stable_cpu_sort_and_validates_inputs() {
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .expect("set DGX_VULKAN_TEST_DEVICE to the selected hardware Vulkan adapter")
            .parse::<usize>()
            .expect("DGX_VULKAN_TEST_DEVICE must be an adapter ordinal");
        let gpu = Context::new(ordinal)
            .unwrap()
            .expect("selected adapter must support subgroup plotting");
        let maximum = 1_048_593;
        let mut sorter = Sorter::new(gpu.clone(), maximum).unwrap();
        let cancelled = AtomicBool::new(false);
        for count in [
            0, 1, 31, 33, 63, 65, 127, 129, 257, 1023, 1024, 1025, 65_537, maximum,
        ] {
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
                let mut input = allocate_entries(&gpu, count, "radix test input").unwrap();
                let mut scratch = allocate_entries(&gpu, count, "radix test scratch").unwrap();
                if count != 0 {
                    gpu.queue
                        .write_buffer(&input[0], 0, bytemuck::cast_slice(&values));
                }
                sorter
                    .sort(&mut input, &mut scratch, count, final_table, &cancelled)
                    .unwrap();
                if count != 0 {
                    let bytes = gpu
                        .read(&input[0], 0, (count * 16) as u64, &cancelled)
                        .unwrap();
                    let actual: Vec<[u32; 4]> =
                        bytemuck::cast_slice::<u8, [u32; 4]>(&bytes).to_vec();
                    assert_eq!(actual, expected, "count={count}, final={final_table}");
                }
            }
        }
        let mut input = allocate_entries(&gpu, 2, "radix invalid input").unwrap();
        let mut scratch = allocate_entries(&gpu, 2, "radix invalid scratch").unwrap();
        assert_eq!(
            sorter
                .sort(&mut input, &mut scratch, 3, false, &cancelled)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        let mut small = Sorter::new(gpu.clone(), 1).unwrap();
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
        for (invalid, final_table) in [([0u32, 0, 1 << 28, 0], false), ([0, 1 << 24, 0, 0], true)] {
            gpu.queue
                .write_buffer(&input[0], 0, bytemuck::cast_slice(&[invalid; 2]));
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
