use crate::{gpu_error, packing_kernels::kernels};
use cuda_core::simt::{LaunchConfig, memory};
use cuda_core::{CudaContext, CudaStream, DeviceBuffer, PinnedHostBuffer};
use dg_xch_plotter::format::{PackedChunk, write_packed_chunks};
use dg_xch_pos2::compact::resident::TRANSFER_BYTES;
use dg_xch_pos2::compute::{allocate, check_cancelled};
use dg_xch_pos2::params::ProofParams;
use std::io::{Error, ErrorKind, Seek, Write};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const CHUNKS: usize = 4096;
const BATCH_CHUNKS: usize = 256;
const OUTPUT_WORDS: usize = TRANSFER_BYTES as usize / size_of::<u32>();
const TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MEMORY_BYTES: u64 = 3 * TRANSFER_BYTES + 16 * 1024 * 1024;

struct Batch {
    descriptors: Vec<[u32; 4]>,
    readback: PinnedHostBuffer<u32>,
    start: usize,
    end: usize,
    words: usize,
}

impl Batch {
    fn new(context: &Arc<CudaContext>) -> Result<Self, Error> {
        Ok(Self {
            descriptors: allocate(BATCH_CHUNKS)?,
            readback: PinnedHostBuffer::zeroed(context, OUTPUT_WORDS).map_err(gpu_error)?,
            start: 0,
            end: 0,
            words: 0,
        })
    }
}

struct Packer<'entries> {
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    module: kernels::LoadedModule,
    entries: &'entries DeviceBuffer<[u32; 4]>,
    count: u32,
    boundaries: Vec<u32>,
    chunks: usize,
    device_descriptors: DeviceBuffer<[u32; 4]>,
    output: DeviceBuffer<u32>,
    errors: DeviceBuffer<u32>,
    slots: [Batch; 2],
    current: usize,
    pending: Option<usize>,
    transferred_bytes: u64,
    batches: usize,
    allocation_seconds: f64,
    enqueue_seconds: f64,
    wait_seconds: f64,
    staging_seconds: f64,
}

impl Drop for Packer<'_> {
    fn drop(&mut self) {
        self.context.record_err(self.stream.synchronize());
    }
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
                    "CUDA plot packing timed out",
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

impl<'entries> Packer<'entries> {
    fn new(
        context: Arc<CudaContext>,
        stream: Arc<CudaStream>,
        entries: &'entries DeviceBuffer<[u32; 4]>,
        count: usize,
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        let started = Instant::now();
        check_cancelled(cancelled)?;
        if !Arc::ptr_eq(&context, stream.context()) || !Arc::ptr_eq(&context, entries.context()) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "CUDA packing buffers belong to another context",
            ));
        }
        if count == 0 || count > entries.len() || count > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid CUDA fragment count",
            ));
        }
        if (entries.num_bytes() as u64)
            .checked_add(MEMORY_BYTES)
            .is_none_or(|required| required > memory_bytes)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "insufficient CUDA packing memory budget",
            ));
        }
        context.bind_to_thread().map_err(gpu_error)?;
        context.check_err().map_err(gpu_error)?;
        let module = kernels::load(&context).map_err(gpu_error)?;
        let errors = DeviceBuffer::zeroed(&stream, 1).map_err(gpu_error)?;
        let device_boundaries =
            DeviceBuffer::<u32>::zeroed(&stream, CHUNKS + 1).map_err(gpu_error)?;
        unsafe {
            module.packing_boundaries(
                &stream,
                LaunchConfig::for_num_elems((CHUNKS + 1) as u32),
                entries,
                count as u32,
                device_boundaries.cu_deviceptr() as *mut u32,
                errors.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        wait(&context, &stream, cancelled)?;
        let mut flags = [0];
        errors
            .copy_to_host(&stream, &mut flags)
            .map_err(gpu_error)?;
        if flags[0] != 0 {
            return Err(Error::other(
                "CUDA fragment boundary kernel rejected its input",
            ));
        }
        let mut boundaries = allocate(CHUNKS + 1)?;
        boundaries.resize(CHUNKS + 1, 0u32);
        device_boundaries
            .copy_to_host(&stream, &mut boundaries)
            .map_err(gpu_error)?;
        if boundaries[0] != 0
            || boundaries[CHUNKS] != count as u32
            || boundaries
                .windows(2)
                .any(|bounds| bounds[0] > bounds[1] || bounds[1] - bounds[0] > 1_048_576)
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid CUDA fragment chunk boundaries",
            ));
        }
        let chunks = boundaries[..CHUNKS].partition_point(|start| *start < count as u32);
        drop(device_boundaries);
        let output = DeviceBuffer::zeroed(&stream, OUTPUT_WORDS).map_err(gpu_error)?;
        let device_descriptors = DeviceBuffer::zeroed(&stream, BATCH_CHUNKS).map_err(gpu_error)?;
        let slots = [Batch::new(&context)?, Batch::new(&context)?];
        wait(&context, &stream, cancelled)?;
        Ok(Self {
            context,
            stream,
            module,
            entries,
            count: count as u32,
            boundaries,
            chunks,
            device_descriptors,
            output,
            errors,
            slots,
            current: 0,
            pending: None,
            transferred_bytes: ((CHUNKS + 2) * size_of::<u32>()) as u64,
            batches: 0,
            allocation_seconds: started.elapsed().as_secs_f64(),
            enqueue_seconds: 0.0,
            wait_seconds: 0.0,
            staging_seconds: 0.0,
        })
    }

    fn enqueue_batch(&mut self, first: usize, cancelled: &AtomicBool) -> Result<(), Error> {
        let started = Instant::now();
        check_cancelled(cancelled)?;
        if self.pending.is_some() {
            return Err(Error::other("CUDA packing already has a pending transfer"));
        }
        let Some(first) = (first..self.chunks)
            .find(|chunk| self.boundaries[*chunk] != self.boundaries[*chunk + 1])
        else {
            return Ok(());
        };
        self.context.bind_to_thread().map_err(gpu_error)?;
        self.context.check_err().map_err(gpu_error)?;
        let slot_index = 1 - self.current;
        let slot = &mut self.slots[slot_index];
        slot.descriptors.clear();
        let mut output_words = 0usize;
        let mut maximum_words = 0usize;
        for chunk in (first..self.chunks).take(BATCH_CHUNKS) {
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
        if slot.descriptors.is_empty() || output_words == 0 {
            return Err(Error::other("CUDA packed chunk exceeds transfer budget"));
        }
        slot.start = first;
        slot.end = first + slot.descriptors.len();
        slot.words = output_words;
        let bytes = output_words * size_of::<u32>();
        if bytes > slot.readback.num_bytes() || bytes > self.output.num_bytes() {
            return Err(Error::other("CUDA packed transfer exceeds its allocation"));
        }
        unsafe {
            memory::memcpy_htod_sync(
                self.device_descriptors.cu_deviceptr(),
                slot.descriptors.as_ptr(),
                size_of_val(slot.descriptors.as_slice()),
            )
        }
        .map_err(gpu_error)?;
        self.errors
            .copy_from_host(&self.stream, &[0])
            .map_err(gpu_error)?;
        self.pending = Some(slot_index);
        unsafe {
            self.module.packing_chunks(
                &self.stream,
                LaunchConfig {
                    grid_dim: (
                        (maximum_words as u32).div_ceil(256),
                        slot.descriptors.len() as u32,
                        1,
                    ),
                    block_dim: (256, 1, 1),
                    shared_mem_bytes: 0,
                },
                self.entries,
                self.count,
                first as u32,
                &self.device_descriptors,
                slot.descriptors.len() as u32,
                self.output.cu_deviceptr() as *mut u32,
                output_words as u32,
                self.errors.cu_deviceptr() as *mut u32,
            )
        }
        .map_err(gpu_error)?;
        unsafe {
            memory::memcpy_dtoh_async(
                slot.readback.as_mut_ptr(),
                self.output.cu_deviceptr(),
                bytes,
                self.stream.cu_stream(),
            )
        }
        .map_err(gpu_error)?;
        self.transferred_bytes += bytes as u64;
        self.batches += 1;
        self.enqueue_seconds += started.elapsed().as_secs_f64();
        check_cancelled(cancelled)
    }

    fn finish_pending(&mut self, cancelled: &AtomicBool) -> Result<(), Error> {
        let Some(slot) = self.pending else {
            return check_cancelled(cancelled);
        };
        let started = Instant::now();
        wait(&self.context, &self.stream, cancelled)?;
        let mut flags = [0];
        self.errors
            .copy_to_host(&self.stream, &mut flags)
            .map_err(gpu_error)?;
        self.transferred_bytes += size_of_val(&flags) as u64;
        self.wait_seconds += started.elapsed().as_secs_f64();
        if flags[0] != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("CUDA fragment packing rejected: flags={:#x}", flags[0]),
            ));
        }
        self.pending = None;
        self.current = slot;
        check_cancelled(cancelled)
    }

    fn load_batch(&mut self, chunk: usize, cancelled: &AtomicBool) -> Result<(), Error> {
        self.finish_pending(cancelled)?;
        if !(self.slots[self.current].start..self.slots[self.current].end).contains(&chunk) {
            self.enqueue_batch(chunk, cancelled)?;
            self.finish_pending(cancelled)?;
        }
        if !(self.slots[self.current].start..self.slots[self.current].end).contains(&chunk) {
            return Err(Error::other("CUDA packed chunk is missing from its batch"));
        }
        self.enqueue_batch(self.slots[self.current].end, cancelled)
    }

    fn chunk(&mut self, chunk: usize, cancelled: &AtomicBool) -> Result<PackedChunk, Error> {
        check_cancelled(cancelled)?;
        if chunk >= self.chunks {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid CUDA packed chunk index",
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
        let descriptor = slot.descriptors[chunk - slot.start];
        let count = descriptor[1] as usize;
        let bytes: &[u8] = bytemuck::cast_slice(&slot.readback.as_slice()[..slot.words]);
        let delta_start = descriptor[2] as usize * size_of::<u32>();
        let stub_start = descriptor[3] as usize * size_of::<u32>();
        let stub_bytes = (count * 26).div_ceil(8);
        let delta_source = bytes
            .get(delta_start..delta_start + count)
            .ok_or_else(|| Error::other("CUDA packed delta range exceeds readback"))?;
        let stub_source = bytes
            .get(stub_start..stub_start + stub_bytes)
            .ok_or_else(|| Error::other("CUDA packed stub range exceeds readback"))?;
        let mut deltas = allocate(count)?;
        deltas.extend_from_slice(delta_source);
        let mut stubs = allocate(stub_bytes)?;
        stubs.extend_from_slice(stub_source);
        self.staging_seconds += started.elapsed().as_secs_f64();
        Ok(PackedChunk {
            count: descriptor[1],
            deltas,
            stubs,
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write(
    output: &mut (impl Write + Seek),
    context: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    entries: &DeviceBuffer<[u32; 4]>,
    count: usize,
    params: &ProofParams,
    index: u16,
    meta_group: u8,
    memo: &[u8],
    memory_bytes: u64,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    if params.k() != 28 || params.strength() != 2 || !cfg!(target_endian = "little") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "CUDA packing requires little-endian k28 strength 2",
        ));
    }
    if !matches!(memo.len(), 112 | 128) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "plot memo must contain 112 or 128 bytes",
        ));
    }
    let started = Instant::now();
    let mut packer = Packer::new(
        context.clone(),
        stream,
        entries,
        count,
        memory_bytes,
        cancelled,
    )?;
    let result = write_packed_chunks(
        output,
        params,
        packer.chunks as u64,
        index,
        meta_group,
        memo,
        cancelled,
        |chunk| packer.chunk(chunk as usize, cancelled),
    );
    let drained = packer.finish_pending(cancelled);
    if std::env::var_os("DGX_POS2_PROFILE").is_some() {
        eprintln!(
            "pos2_cuda_packing seconds={:.3} chunks={} batches={} d2h_bytes={} allocation_seconds={:.3} host_enqueue_seconds={:.3} host_wait_seconds={:.3} staging_seconds={:.3}",
            started.elapsed().as_secs_f64(),
            packer.chunks,
            packer.batches,
            packer.transferred_bytes,
            packer.allocation_seconds,
            packer.enqueue_seconds,
            packer.wait_seconds,
            packer.staging_seconds,
        );
    }
    drop(packer);
    let cleanup = context.check_err().map_err(gpu_error);
    result?;
    drained?;
    cleanup
}

#[cfg(test)]
mod tests {
    use super::*;
    use dg_xch_pos2::compact::CompactPlot;
    use std::io::Cursor;

    struct FailingWriter {
        output: Cursor<Vec<u8>>,
        boundary: u64,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> Result<usize, Error> {
            if self.output.position() >= self.boundary {
                return Err(Error::other("injected packed writer failure"));
            }
            self.output.write(bytes)
        }

        fn flush(&mut self) -> Result<(), Error> {
            self.output.flush()
        }
    }

    impl Seek for FailingWriter {
        fn seek(&mut self, position: std::io::SeekFrom) -> Result<u64, Error> {
            self.output.seek(position)
        }
    }

    fn words(fragments: &[u64]) -> Vec<[u32; 4]> {
        fragments
            .iter()
            .map(|value| [*value as u32, (*value >> 32) as u32, 0, 0])
            .collect()
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
        if pending != 0 {
            stubs.push(buffer as u8);
        }
        PackedChunk {
            count: fragments.len() as u32,
            deltas,
            stubs,
        }
    }

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

    #[test]
    fn k28_packing_memory_and_word_boundaries_are_bounded() {
        assert_eq!(MEMORY_BYTES, 208 * 1024 * 1024);
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
    #[ignore = "requires an explicit DGX_CUDA_TEST_DEVICE and a hardware CUDA GPU"]
    fn k28_cuda_packing_matches_canonical_file_and_rejects_invalid_fragments() {
        let ordinal = std::env::var("DGX_CUDA_TEST_DEVICE")
            .expect("set DGX_CUDA_TEST_DEVICE to the selected hardware CUDA adapter")
            .parse::<usize>()
            .expect("DGX_CUDA_TEST_DEVICE must be an adapter ordinal");
        let context = CudaContext::new(ordinal).unwrap();
        context.bind_to_thread().unwrap();
        let stream = context.default_stream();
        let cancelled = AtomicBool::new(false);
        let fragments = fixture();
        let params = ProofParams::new([37; 32].into(), 28, 2, false).unwrap();
        let input = DeviceBuffer::from_host(&stream, &words(&fragments)).unwrap();
        let budget = input.num_bytes() as u64 + MEMORY_BYTES;
        {
            let mut packer = Packer::new(
                context.clone(),
                stream.clone(),
                &input,
                fragments.len(),
                budget,
                &cancelled,
            )
            .unwrap();
            assert_eq!(packer.chunks, CHUNKS);
            for chunk in 0..CHUNKS {
                let start = fragments.partition_point(|value| *value >> 44 < chunk as u64);
                let end = fragments.partition_point(|value| *value >> 44 <= chunk as u64);
                let expected = cpu_chunk(chunk as u64, &fragments[start..end]);
                let actual = packer.chunk(chunk, &cancelled).unwrap();
                if chunk == 0 {
                    assert!(packer.pending.is_some());
                }
                assert_eq!(actual.count, expected.count, "chunk {chunk}");
                assert_eq!(actual.deltas, expected.deltas, "chunk {chunk}");
                assert_eq!(actual.stubs, expected.stubs, "chunk {chunk}");
            }
        }
        let plot = CompactPlot::from_sorted_fragments(
            params.clone(),
            fragments.clone(),
            [1 << 28, 0, 0, fragments.len()],
            &cancelled,
        )
        .unwrap();
        for memo in [vec![0; 112], vec![0; 128]] {
            let mut expected = Cursor::new(Vec::new());
            dg_xch_plotter::format::write_compact(
                &mut expected,
                &plot,
                u16::MAX,
                u8::MAX,
                &memo,
                &cancelled,
            )
            .unwrap();
            let mut actual = Cursor::new(Vec::new());
            write(
                &mut actual,
                context.clone(),
                stream.clone(),
                &input,
                fragments.len(),
                &params,
                u16::MAX,
                u8::MAX,
                &memo,
                budget,
                &cancelled,
            )
            .unwrap();
            assert_eq!(actual.get_ref(), expected.get_ref());
        }
        let mut failed_output = FailingWriter {
            output: Cursor::new(Vec::new()),
            boundary: 43 + 112 + 8 + CHUNKS as u64 * 8,
        };
        let error = write(
            &mut failed_output,
            context.clone(),
            stream.clone(),
            &input,
            fragments.len(),
            &params,
            0,
            0,
            &[0; 112],
            budget,
            &cancelled,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "injected packed writer failure");
        {
            let interrupted = AtomicBool::new(false);
            let mut packer = Packer::new(
                context.clone(),
                stream.clone(),
                &input,
                fragments.len(),
                budget,
                &interrupted,
            )
            .unwrap();
            packer.chunk(0, &interrupted).unwrap();
            assert!(packer.pending.is_some());
            interrupted.store(true, std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                packer.chunk(1, &interrupted).err().unwrap().kind(),
                ErrorKind::Interrupted
            );
        }
        context.check_err().unwrap();
        assert!(
            Packer::new(
                context.clone(),
                stream.clone(),
                &input,
                fragments.len(),
                budget - 1,
                &cancelled
            )
            .is_err()
        );
        assert_eq!(
            Packer::new(
                context.clone(),
                stream.clone(),
                &input,
                fragments.len(),
                budget,
                &AtomicBool::new(true)
            )
            .err()
            .unwrap()
            .kind(),
            ErrorKind::Interrupted
        );
        for invalid in [
            vec![1u64],
            vec![1, 2],
            vec![0, 2, 1],
            vec![0, 0, 1 << 34],
            vec![1 << 56],
        ] {
            let input = DeviceBuffer::from_host(&stream, &words(&invalid)).unwrap();
            let mut output = Cursor::new(Vec::new());
            let result = write(
                &mut output,
                context.clone(),
                stream.clone(),
                &input,
                invalid.len(),
                &params,
                0,
                0,
                &[0; 112],
                input.num_bytes() as u64 + MEMORY_BYTES,
                &cancelled,
            );
            assert!(result.is_err(), "invalid fragments {invalid:?} must fail");
        }
        context.check_err().unwrap();
    }
}
