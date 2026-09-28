use crate::compact::PackedChunk;
use crate::compact::resident::TRANSFER_BYTES;
use crate::compute::{allocate, check_cancelled};
use crate::vulkan_full::{Context, DevicePlot};
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CHUNKS: usize = 4096;
const SHARDS: usize = 5;
const SHARD_ENTRIES: usize = 1 << 26;
const BATCH_CHUNKS: usize = 256;
const OUTPUT_WORDS: usize = TRANSFER_BYTES as usize / size_of::<u32>();
const TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MEMORY_BYTES: u64 = 3 * TRANSFER_BYTES + 16 * 1024 * 1024;

fn memory_required(input_bytes: u64) -> Result<u64, Error> {
    input_bytes
        .checked_add(MEMORY_BYTES)
        .ok_or_else(|| Error::other("Vulkan packing memory budget overflow"))
}

struct Batch {
    descriptors: Vec<[u32; 4]>,
    readback: wgpu::Buffer,
    completion: Option<mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>>,
    mapped: bool,
    start: usize,
    end: usize,
    words: usize,
}

impl Batch {
    fn new(gpu: &Context) -> Result<Self, Error> {
        let descriptors = allocate(BATCH_CHUNKS)?;
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PoS2 packed readback slot"),
            size: TRANSFER_BYTES + 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.check()?;
        Ok(Self {
            descriptors,
            readback,
            completion: None,
            mapped: false,
            start: 0,
            end: 0,
            words: 0,
        })
    }

    fn mapped_bytes(&self) -> u64 {
        (16 + self.words as u64 * 4).div_ceil(8) * 8
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        if self.mapped || self.completion.is_some() {
            self.readback.unmap();
        }
    }
}

pub struct PackedReader<'plot> {
    plot: &'plot DevicePlot,
    configuration: wgpu::Buffer,
    device_descriptors: wgpu::Buffer,
    output: wgpu::Buffer,
    errors: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    pipeline: wgpu::ComputePipeline,
    boundaries: Vec<u32>,
    chunk_count: usize,
    slots: [Batch; 2],
    current: usize,
    pending: Option<usize>,
    transferred_bytes: u64,
    batches: usize,
    started: Instant,
    allocation_seconds: f64,
    enqueue_seconds: f64,
    wait_seconds: f64,
    staging_seconds: f64,
}

impl DevicePlot {
    pub fn packed_chunks(
        &self,
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<PackedReader<'_>, Error> {
        PackedReader::new(self, memory_bytes, cancelled)
    }
}

impl Drop for PackedReader<'_> {
    fn drop(&mut self) {
        if std::env::var_os("DGX_POS2_PROFILE").is_some() {
            eprintln!(
                "pos2_vulkan_packing seconds={:.3} chunks={} batches={} d2h_bytes={} allocation_seconds={:.3} host_enqueue_seconds={:.3} host_wait_seconds={:.3} staging_seconds={:.3}",
                self.started.elapsed().as_secs_f64(),
                self.chunk_count,
                self.batches,
                self.transferred_bytes,
                self.allocation_seconds,
                self.enqueue_seconds,
                self.wait_seconds,
                self.staging_seconds,
            );
        }
    }
}

impl<'plot> PackedReader<'plot> {
    fn new(
        plot: &'plot DevicePlot,
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        let started = Instant::now();
        check_cancelled(cancelled)?;
        if plot.params.k() != 28 || plot.params.strength() != 2 || !cfg!(target_endian = "little") {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Vulkan packing requires little-endian k28 strength 2",
            ));
        }
        if plot.count == 0
            || plot.count > plot.capacity
            || plot.count > u32::MAX as usize
            || plot.count > SHARDS * SHARD_ENTRIES
            || plot.entries.len() != SHARDS
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid Vulkan fragment count or shards",
            ));
        }
        let mut input_bytes = 0u64;
        for (ordinal, shard) in plot.entries.iter().enumerate() {
            let required = plot
                .count
                .saturating_sub(ordinal * SHARD_ENTRIES)
                .min(SHARD_ENTRIES) as u64
                * 16;
            if shard.size() < required.max(16) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Vulkan fragment shard is too small",
                ));
            }
            input_bytes = input_bytes
                .checked_add(shard.size())
                .ok_or_else(|| Error::other("Vulkan input size overflow"))?;
        }
        if memory_required(input_bytes)? > memory_bytes {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "insufficient Vulkan packing memory budget",
            ));
        }
        let gpu = &plot.gpu;
        gpu.check()?;
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut layout_entries = allocate(10)?;
        for binding in 0..10 {
            layout_entries.push(wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: if binding == 0 {
                        wgpu::BufferBindingType::Uniform
                    } else {
                        wgpu::BufferBindingType::Storage {
                            read_only: matches!(binding, 1..=5 | 7),
                        }
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            });
        }
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("PoS2 packed writer bindings"),
                entries: &layout_entries,
            });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PoS2 packed writer pipeline"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("PoS2 packed writer shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("vulkan_packing.wgsl").into()),
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
        let boundary_pipeline = pipeline("packing_boundaries");
        let pipeline = pipeline("packing_chunks");
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::other(format!("Vulkan packing shader: {error}")));
        }
        let buffer = |label, size, usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let configuration = buffer(
            "PoS2 packing configuration",
            16,
            wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        );
        let device_boundaries = buffer(
            "PoS2 packing boundaries",
            (CHUNKS as u64 + 1) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let device_descriptors = buffer(
            "PoS2 packing descriptors",
            BATCH_CHUNKS as u64 * 16,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        let output = buffer(
            "PoS2 packed output",
            TRANSFER_BYTES,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let errors = buffer(
            "PoS2 packing flags",
            16,
            wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
        );
        let buffers = [
            &configuration,
            &plot.entries[0],
            &plot.entries[1],
            &plot.entries[2],
            &plot.entries[3],
            &plot.entries[4],
            &device_boundaries,
            &device_descriptors,
            &output,
            &errors,
        ];
        let mut binding_entries = allocate(buffers.len())?;
        for (binding, buffer) in buffers.iter().enumerate() {
            binding_entries.push(wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: buffer.as_entire_binding(),
            });
        }
        let bindings = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("PoS2 packing input tables"),
            layout: &layout,
            entries: &binding_entries,
        });
        gpu.check()?;
        gpu.queue.write_buffer(
            &configuration,
            0,
            bytemuck::cast_slice(&[plot.count as u32, 0, 0, 0]),
        );
        gpu.queue.write_buffer(&errors, 0, &[0; 16]);
        let mut encoder = gpu.encoder("PoS2 fragment boundaries");
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&boundary_pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups((CHUNKS as u32 + 1).div_ceil(256), 1, 1);
        }
        gpu.wait(gpu.queue.submit([encoder.finish()]), cancelled)?;
        let flags = gpu.read(&errors, 0, 16, cancelled)?;
        if flags.len() != 16 || flags[..4] != [0; 4] {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "Vulkan fragment boundary kernel rejected its input",
            ));
        }
        let bytes = gpu.read(&device_boundaries, 0, (CHUNKS as u64 + 1) * 4, cancelled)?;
        if bytes.len() != (CHUNKS + 1) * 4 {
            return Err(Error::other(
                "Vulkan fragment boundary readback length mismatch",
            ));
        }
        let mut boundaries = allocate(CHUNKS + 1)?;
        boundaries.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word)),
        );
        if boundaries[0] != 0
            || boundaries[CHUNKS] != plot.count as u32
            || boundaries
                .windows(2)
                .any(|bounds| bounds[0] > bounds[1] || bounds[1] - bounds[0] > 1_048_576)
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid Vulkan fragment chunk boundaries",
            ));
        }
        let chunk_count = boundaries[..CHUNKS].partition_point(|start| *start < plot.count as u32);
        let slots = [Batch::new(gpu)?, Batch::new(gpu)?];
        check_cancelled(cancelled)?;
        gpu.check()?;
        Ok(Self {
            plot,
            configuration,
            device_descriptors,
            output,
            errors,
            bindings,
            pipeline,
            boundaries,
            chunk_count,
            slots,
            current: 0,
            pending: None,
            transferred_bytes: ((CHUNKS + 1) * 4 + 16) as u64,
            batches: 0,
            started,
            allocation_seconds: started.elapsed().as_secs_f64(),
            enqueue_seconds: 0.0,
            wait_seconds: 0.0,
            staging_seconds: 0.0,
        })
    }

    pub fn chunks(&self) -> u64 {
        self.chunk_count as u64
    }

    fn enqueue_batch(&mut self, first: usize, cancelled: &AtomicBool) -> Result<(), Error> {
        let started = Instant::now();
        check_cancelled(cancelled)?;
        if self.pending.is_some() {
            return Err(Error::other(
                "Vulkan packing already has a pending transfer",
            ));
        }
        let Some(first) = (first..self.chunk_count)
            .find(|chunk| self.boundaries[*chunk] != self.boundaries[*chunk + 1])
        else {
            return Ok(());
        };
        let gpu = &self.plot.gpu;
        gpu.check()?;
        let slot_index = 1 - self.current;
        let slot = &mut self.slots[slot_index];
        if slot.mapped {
            slot.readback.unmap();
            slot.mapped = false;
        }
        slot.descriptors.clear();
        let mut output_words = 0usize;
        let mut maximum_words = 0usize;
        for chunk in (first..self.chunk_count).take(BATCH_CHUNKS) {
            check_cancelled(cancelled)?;
            let start = self.boundaries[chunk];
            let count = self.boundaries[chunk + 1] - start;
            let delta_words = (count as usize).div_ceil(4);
            let stub_words = (count as usize * 26).div_ceil(32);
            let words = delta_words + stub_words;
            if words > OUTPUT_WORDS - output_words {
                break;
            }
            slot.descriptors.push([
                start,
                count,
                output_words as u32,
                (output_words + delta_words) as u32,
            ]);
            output_words += words;
            maximum_words = maximum_words.max(words);
        }
        if output_words == 0 {
            return Err(Error::other("Vulkan packed chunk exceeds transfer budget"));
        }
        slot.start = first;
        slot.end = first + slot.descriptors.len();
        slot.words = output_words;
        gpu.queue.write_buffer(
            &self.configuration,
            0,
            bytemuck::cast_slice(&[
                self.plot.count as u32,
                first as u32,
                slot.descriptors.len() as u32,
                output_words as u32,
            ]),
        );
        gpu.queue.write_buffer(
            &self.device_descriptors,
            0,
            bytemuck::cast_slice(&slot.descriptors),
        );
        gpu.queue.write_buffer(&self.errors, 0, &[0; 16]);
        let mut encoder = gpu.encoder("PoS2 packed fragment transfer");
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.dispatch_workgroups(
                (maximum_words as u32).div_ceil(256),
                slot.descriptors.len() as u32,
                1,
            );
        }
        encoder.copy_buffer_to_buffer(&self.errors, 0, &slot.readback, 0, 16);
        encoder.copy_buffer_to_buffer(&self.output, 0, &slot.readback, 16, output_words as u64 * 4);
        gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        slot.completion = Some(receiver);
        self.pending = Some(slot_index);
        slot.readback
            .slice(..slot.mapped_bytes())
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.transferred_bytes += output_words as u64 * 4 + 16;
        self.batches += 1;
        self.enqueue_seconds += started.elapsed().as_secs_f64();
        gpu.check()?;
        check_cancelled(cancelled)
    }

    fn finish_pending(&mut self, cancelled: &AtomicBool) -> Result<(), Error> {
        let Some(slot_index) = self.pending else {
            return check_cancelled(cancelled);
        };
        let started = Instant::now();
        let gpu = &self.plot.gpu;
        let slot = &mut self.slots[slot_index];
        let receiver = slot
            .completion
            .as_ref()
            .ok_or_else(|| Error::other("Vulkan packed transfer has no completion receiver"))?;
        loop {
            check_cancelled(cancelled)?;
            gpu.check()?;
            gpu.device
                .poll(wgpu::PollType::Poll)
                .map_err(|error| Error::other(format!("Vulkan packing polling: {error}")))?;
            match receiver.try_recv() {
                Ok(result) => {
                    result.map_err(|error| {
                        Error::other(format!("Vulkan packing mapping: {error}"))
                    })?;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(Error::other("Vulkan packing mapping channel disconnected"));
                }
            }
            if started.elapsed() >= TIMEOUT {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "Vulkan packing readback timed out",
                ));
            }
            std::thread::sleep(Duration::from_micros(100));
        }
        slot.completion = None;
        slot.mapped = true;
        let flags = {
            let view = slot
                .readback
                .slice(..16)
                .get_mapped_range()
                .map_err(|error| Error::other(format!("Vulkan packed status view: {error}")))?;
            if view.len() < 4 {
                return Err(Error::other("Vulkan packed status readback is too short"));
            }
            u32::from_le_bytes([view[0], view[1], view[2], view[3]])
        };
        self.wait_seconds += started.elapsed().as_secs_f64();
        if flags != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Vulkan fragment packing rejected: flags={flags:#x}"),
            ));
        }
        self.pending = None;
        self.current = slot_index;
        gpu.check()?;
        check_cancelled(cancelled)
    }

    fn load_batch(&mut self, chunk: usize, cancelled: &AtomicBool) -> Result<(), Error> {
        self.finish_pending(cancelled)?;
        if !(self.slots[self.current].start..self.slots[self.current].end).contains(&chunk) {
            self.enqueue_batch(chunk, cancelled)?;
            self.finish_pending(cancelled)?;
        }
        if !(self.slots[self.current].start..self.slots[self.current].end).contains(&chunk) {
            return Err(Error::other(
                "Vulkan packed chunk is missing from its batch",
            ));
        }
        self.enqueue_batch(self.slots[self.current].end, cancelled)
    }

    pub fn chunk(&mut self, index: u64, cancelled: &AtomicBool) -> Result<PackedChunk, Error> {
        check_cancelled(cancelled)?;
        self.plot.gpu.check()?;
        let chunk = usize::try_from(index).map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                "Vulkan chunk index exceeds address space",
            )
        })?;
        if chunk >= self.chunk_count {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid Vulkan packed chunk index",
            ));
        }
        if self.boundaries[chunk] == self.boundaries[chunk + 1] {
            return Ok(PackedChunk {
                count: 0,
                deltas: Vec::new(),
                stubs: Vec::new(),
            });
        }
        if !(self.slots[self.current].start..self.slots[self.current].end).contains(&chunk) {
            self.load_batch(chunk, cancelled)?;
        }
        let started = Instant::now();
        let slot = &self.slots[self.current];
        if !slot.mapped {
            return Err(Error::other("Vulkan packed batch is not mapped"));
        }
        let descriptor = slot.descriptors[chunk - slot.start];
        let count = descriptor[1] as usize;
        let view = slot
            .readback
            .slice(..slot.mapped_bytes())
            .get_mapped_range()
            .map_err(|error| Error::other(format!("Vulkan packed output view: {error}")))?;
        let delta_start = 16 + descriptor[2] as usize * 4;
        let stub_start = 16 + descriptor[3] as usize * 4;
        let stub_bytes = (count * 26).div_ceil(8);
        let delta_source = view
            .get(delta_start..delta_start + count)
            .ok_or_else(|| Error::other("Vulkan packed delta range exceeds readback"))?;
        let stub_source = view
            .get(stub_start..stub_start + stub_bytes)
            .ok_or_else(|| Error::other("Vulkan packed stub range exceeds readback"))?;
        let mut deltas = allocate(count)?;
        deltas.extend_from_slice(delta_source);
        let mut stubs = allocate(stub_bytes)?;
        stubs.extend_from_slice(stub_source);
        drop(view);
        self.staging_seconds += started.elapsed().as_secs_f64();
        check_cancelled(cancelled)?;
        Ok(PackedChunk {
            count: descriptor[1],
            deltas,
            stubs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::ProofParams;
    use crate::vulkan_full::allocate_entries;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    fn fixture() -> Vec<u64> {
        let mut fragments = Vec::new();
        for (chunk, count) in [(0u64, 257usize), (2, 1025), (4095, 65_537)] {
            let mut previous = chunk << 44;
            for position in 0..count {
                let delta = if position % 31 == 0 {
                    0
                } else {
                    ((if position % 17 == 0 { 4u64 } else { 1 }) << 26)
                        | ((position as u64 * 0x0123_4567) & ((1 << 26) - 1))
                };
                previous += delta;
                assert_eq!(previous >> 44, chunk);
                fragments.push(previous);
            }
        }
        fragments
    }

    fn cpu_chunk(chunk: u64, fragments: &[u64]) -> PackedChunk {
        let mut previous = chunk << 44;
        let mut deltas = Vec::new();
        let mut stubs = Vec::new();
        let mut buffer = 0u64;
        let mut pending = 0;
        for &fragment in fragments {
            let delta = fragment - previous;
            previous = fragment;
            deltas.push(u8::try_from(delta >> 26).unwrap());
            buffer |= (delta & ((1 << 26) - 1)) << pending;
            pending += 26;
            while pending >= 8 {
                stubs.push(buffer as u8);
                buffer >>= 8;
                pending -= 8;
            }
        }
        if pending > 0 {
            stubs.push(buffer as u8);
        }
        PackedChunk {
            count: fragments.len() as u32,
            deltas,
            stubs,
        }
    }

    fn plot(gpu: Arc<Context>, fragments: &[u64]) -> DevicePlot {
        let entries = allocate_entries(&gpu, fragments.len(), "Vulkan packed fixture").unwrap();
        let words: Vec<_> = fragments
            .iter()
            .map(|value| [*value as u32, (*value >> 32) as u32, 0, 0])
            .collect();
        if !words.is_empty() {
            gpu.queue
                .write_buffer(&entries[0], 0, bytemuck::cast_slice(&words));
        }
        DevicePlot {
            gpu,
            entries,
            count: fragments.len(),
            capacity: fragments.len(),
            params: ProofParams::new([37; 32].into(), 28, 2, false).unwrap(),
            table_counts: [1 << 28, 0, 0, fragments.len()],
        }
    }

    fn budget(plot: &DevicePlot) -> u64 {
        memory_required(plot.entries.iter().map(wgpu::Buffer::size).sum()).unwrap()
    }

    #[test]
    fn k28_vulkan_packing_memory_and_word_boundaries_are_bounded() {
        assert_eq!(MEMORY_BYTES, 208 * 1024 * 1024);
        assert_eq!(memory_required(0).unwrap(), MEMORY_BYTES);
        assert_eq!(memory_required(5 << 30).unwrap(), (5 << 30) + MEMORY_BYTES);
        assert!(memory_required(u64::MAX).is_err());
        assert_eq!(OUTPUT_WORDS * size_of::<u32>(), TRANSFER_BYTES as usize);
        for count in [0usize, 1, 2, 3, 31, 32, 33, 257, 1_048_576] {
            let delta_words = count.div_ceil(4);
            let stub_words = (count * 26).div_ceil(32);
            assert!(delta_words * 4 >= count);
            assert!(stub_words * 4 >= (count * 26).div_ceil(8));
            assert!(delta_words + stub_words <= OUTPUT_WORDS);
        }
    }

    #[test]
    #[cfg(target_endian = "little")]
    #[ignore = "requires an explicit DGX_VULKAN_TEST_DEVICE and a hardware Vulkan GPU"]
    fn k28_vulkan_packing_matches_cpu_and_rejects_invalid_fragments() {
        let ordinal = std::env::var("DGX_VULKAN_TEST_DEVICE")
            .expect("set DGX_VULKAN_TEST_DEVICE to the selected hardware Vulkan adapter")
            .parse::<usize>()
            .expect("DGX_VULKAN_TEST_DEVICE must be an adapter ordinal");
        let gpu = Context::new(ordinal)
            .unwrap()
            .expect("selected adapter must support device-resident plotting");
        let cancelled = AtomicBool::new(false);
        let fragments = fixture();
        let fixture = plot(gpu.clone(), &fragments);
        {
            let mut reader = fixture.packed_chunks(budget(&fixture), &cancelled).unwrap();
            assert_eq!(reader.chunks(), CHUNKS as u64);
            for chunk in 0..CHUNKS {
                let start = fragments.partition_point(|value| *value >> 44 < chunk as u64);
                let end = fragments.partition_point(|value| *value >> 44 <= chunk as u64);
                let expected = cpu_chunk(chunk as u64, &fragments[start..end]);
                let actual = reader.chunk(chunk as u64, &cancelled).unwrap();
                if chunk == 0 {
                    assert!(reader.pending.is_some());
                }
                assert_eq!(actual.count, expected.count, "chunk {chunk}");
                assert_eq!(actual.deltas, expected.deltas, "chunk {chunk}");
                assert_eq!(actual.stubs, expected.stubs, "chunk {chunk}");
            }
            assert!(reader.chunk(CHUNKS as u64, &cancelled).is_err());
        }
        assert!(
            fixture
                .packed_chunks(budget(&fixture) - 1, &cancelled)
                .is_err()
        );
        assert_eq!(
            fixture
                .packed_chunks(budget(&fixture), &AtomicBool::new(true))
                .err()
                .unwrap()
                .kind(),
            ErrorKind::Interrupted
        );
        {
            let interrupted = AtomicBool::new(false);
            let mut reader = fixture
                .packed_chunks(budget(&fixture), &interrupted)
                .unwrap();
            reader.chunk(0, &interrupted).unwrap();
            assert!(reader.pending.is_some());
            interrupted.store(true, Ordering::Relaxed);
            assert_eq!(
                reader.chunk(1, &interrupted).err().unwrap().kind(),
                ErrorKind::Interrupted
            );
        }
        for fragments in [vec![0u64, 2, 1], vec![0, 0, 1 << 34], vec![1 << 56]] {
            let fixture = plot(gpu.clone(), &fragments);
            let result = fixture
                .packed_chunks(budget(&fixture), &cancelled)
                .and_then(|mut reader| reader.chunk(0, &cancelled));
            assert!(result.is_err(), "invalid fragments {fragments:?} must fail");
        }
        for fragments in [vec![1u64], vec![1, 2]] {
            let fixture = plot(gpu.clone(), &fragments);
            let actual = fixture
                .packed_chunks(budget(&fixture), &cancelled)
                .unwrap()
                .chunk(0, &cancelled)
                .unwrap();
            let expected = cpu_chunk(0, &fragments);
            assert_eq!(actual.count, expected.count);
            assert_eq!(actual.deltas, expected.deltas);
            assert_eq!(actual.stubs, expected.stubs);
        }
        gpu.wait(gpu.queue.submit([]), &cancelled).unwrap();
        gpu.check().unwrap();
    }
}
