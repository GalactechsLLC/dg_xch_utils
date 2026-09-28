use cuda_device::atomic::{AtomicOrdering, DeviceAtomicU32};
use cuda_device::{cuda_module, kernel, thread};

const CHUNKS: usize = 4096;
const SPAN_BITS: u32 = 44;
const STUB_BITS: u32 = 26;
const STUB_MASK: u64 = (1u64 << STUB_BITS) - 1;

#[inline]
fn fragment(entry: [u32; 4]) -> u64 {
    u64::from(entry[0]) | (u64::from(entry[1]) << 32)
}

#[inline]
fn delta(
    entries: &[[u32; 4]],
    position: usize,
    first: usize,
    chunk: u32,
    errors: &DeviceAtomicU32,
) -> u64 {
    let current = fragment(entries[position]);
    let previous = if position == first {
        u64::from(chunk) << SPAN_BITS
    } else {
        fragment(entries[position - 1])
    };
    if current >> SPAN_BITS != u64::from(chunk) || current < previous {
        errors.fetch_or(1, AtomicOrdering::Relaxed);
        return 0;
    }
    let difference = current - previous;
    if difference >> STUB_BITS > 255 {
        errors.fetch_or(2, AtomicOrdering::Relaxed);
        return 0;
    }
    difference
}

#[cuda_module]
pub mod kernels {
    use super::*;

    #[kernel]
    pub fn packing_boundaries(
        entries: &[[u32; 4]],
        count: u32,
        boundaries: *mut u32,
        errors: *mut u32,
    ) {
        let chunk = thread::index_1d().get();
        if chunk > CHUNKS {
            return;
        }
        if count as usize > entries.len() {
            unsafe { DeviceAtomicU32::from_ptr(errors) }.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let wanted = (chunk as u64) << SPAN_BITS;
        let mut lower = 0usize;
        let mut upper = count as usize;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if fragment(entries[middle]) < wanted {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        unsafe {
            boundaries.add(chunk).write(lower as u32);
        }
    }

    #[kernel]
    pub fn packing_chunks(
        entries: &[[u32; 4]],
        count: u32,
        first_chunk: u32,
        descriptors: &[[u32; 4]],
        descriptor_count: u32,
        output: *mut u32,
        output_words: u32,
        errors: *mut u32,
    ) {
        let descriptor_index = thread::blockIdx_y() as usize;
        if descriptor_index >= descriptor_count as usize {
            return;
        }
        let errors = unsafe { DeviceAtomicU32::from_ptr(errors) };
        if descriptor_index >= descriptors.len() {
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let descriptor = descriptors[descriptor_index];
        let first = descriptor[0] as usize;
        let values = descriptor[1] as usize;
        let delta_words = (values + 3) / 4;
        let stub_words = (values * STUB_BITS as usize + 31) / 32;
        let total_words = delta_words + stub_words;
        let chunk = first_chunk + descriptor_index as u32;
        if chunk as usize >= CHUNKS
            || values > 1_048_576
            || first > count as usize
            || values > count as usize - first
            || count as usize > entries.len()
            || u64::from(descriptor[2]) + delta_words as u64 != u64::from(descriptor[3])
            || u64::from(descriptor[2]) + total_words as u64 > u64::from(output_words)
        {
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let word_index = thread::blockIdx_x() as usize * thread::blockDim_x() as usize
            + thread::threadIdx_x() as usize;
        if word_index >= total_words {
            return;
        }
        let mut packed = 0u64;
        if word_index < delta_words {
            for lane in 0..4usize {
                let relative = word_index * 4 + lane;
                if relative < values {
                    let difference = delta(entries, first + relative, first, chunk, errors);
                    packed |= (difference >> STUB_BITS) << (lane * 8);
                }
            }
        } else {
            let first_bit = (word_index - delta_words) * 32;
            let mut relative = first_bit / STUB_BITS as usize;
            let skipped = first_bit % STUB_BITS as usize;
            if relative < values {
                packed =
                    (delta(entries, first + relative, first, chunk, errors) & STUB_MASK) >> skipped;
            }
            relative += 1;
            let mut pending = STUB_BITS as usize - skipped;
            while pending < 32 && relative < values {
                packed |=
                    (delta(entries, first + relative, first, chunk, errors) & STUB_MASK) << pending;
                pending += STUB_BITS as usize;
                relative += 1;
            }
        }
        unsafe {
            output
                .add(descriptor[2] as usize + word_index)
                .write(packed as u32);
        }
    }
}
