use cuda_device::atomic::{AtomicOrdering, BlockAtomicU32, DeviceAtomicU32};
use cuda_device::{SharedArray, cuda_module, kernel, thread, warp};

const THREADS: usize = 256;
const ITEMS: usize = 2048;
const BINS: usize = 256;
const WARPS: usize = 8;
const PREFIX_ITEMS: usize = 1024;

#[inline]
fn digit(entry: [u32; 4], shift: u32, final_table: u32) -> usize {
    if final_table == 0 {
        ((entry[2] >> shift) & 255) as usize
    } else if shift < 32 {
        ((entry[0] >> shift) & 255) as usize
    } else {
        ((entry[1] >> (shift - 32)) & 255) as usize
    }
}

#[inline]
fn warp_inclusive(mut value: u32) -> u32 {
    let lane = thread::threadIdx_x() & 31;
    let mut distance = 1;
    while distance < 32 {
        let previous = warp::shuffle_up_sync(u32::MAX, value, distance);
        if lane >= distance {
            value = value.wrapping_add(previous);
        }
        distance *= 2;
    }
    value
}

#[inline]
fn block_inclusive(value: u32, totals: *mut u32) -> u32 {
    let local = thread::threadIdx_x() as usize;
    let lane = local & 31;
    let warp_index = local >> 5;
    let inclusive = warp_inclusive(value);
    if lane == 31 {
        unsafe { totals.add(warp_index).write(inclusive) };
    }
    thread::sync_threads();
    if warp_index == 0 {
        let warp_total = if lane < WARPS {
            unsafe { totals.add(lane).read() }
        } else {
            0
        };
        let prefix = warp_inclusive(warp_total);
        if lane < WARPS {
            unsafe { totals.add(lane).write(prefix) };
        }
    }
    thread::sync_threads();
    let preceding = if warp_index == 0 {
        0
    } else {
        unsafe { totals.add(warp_index - 1).read() }
    };
    let result = inclusive.wrapping_add(preceding);
    thread::sync_threads();
    result
}

#[cuda_module]
pub mod kernels {
    use super::*;

    #[kernel]
    pub fn radix_histogram(
        input: *const [u32; 4],
        count: u32,
        blocks: u32,
        shift: u32,
        final_table: u32,
        histogram: *mut u32,
        errors: *mut u32,
    ) {
        static mut HISTOGRAM: SharedArray<u32, { WARPS * BINS }> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut HISTOGRAM) };
        let local = thread::threadIdx_x() as usize;
        let warp_index = local >> 5;
        let lane = local & 31;
        let block = thread::blockIdx_x() as usize;
        for offset in 0..WARPS {
            unsafe { shared.add(offset * BINS + local).write(0) };
        }
        thread::sync_threads();
        for iteration in 0..8 {
            let position = block * ITEMS + warp_index * 256 + iteration * 32 + lane;
            let valid = position < count as usize;
            let entry = if valid {
                unsafe { input.add(position).read() }
            } else {
                [0; 4]
            };
            let bucket = digit(entry, shift, final_table);
            let active = warp::ballot_sync(u32::MAX, valid);
            let matching = warp::match_any_sync(u32::MAX, bucket as u32) & active;
            if valid && lane as u32 == matching.trailing_zeros() {
                let counter =
                    unsafe { BlockAtomicU32::from_ptr(shared.add(warp_index * BINS + bucket)) };
                counter.fetch_add(matching.count_ones(), AtomicOrdering::Relaxed);
            }
            if valid
                && ((final_table == 0 && entry[2] >= 1 << 28)
                    || (final_table != 0 && entry[1] >= 1 << 24))
            {
                unsafe { DeviceAtomicU32::from_ptr(errors) }.fetch_or(1, AtomicOrdering::Relaxed);
            }
        }
        thread::sync_threads();
        let mut total = 0u32;
        for warp_index in 0..WARPS {
            total = total.wrapping_add(unsafe { shared.add(warp_index * BINS + local).read() });
        }
        unsafe { histogram.add(local * blocks as usize + block).write(total) };
    }

    #[kernel]
    pub fn radix_prefix_chunks(histogram: *mut u32, blocks: u32, chunks: u32, sums: *mut u32) {
        static mut TOTALS: SharedArray<u32, WARPS> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut TOTALS) };
        let local = thread::threadIdx_x() as usize;
        let chunk = thread::blockIdx_x() as usize;
        let bucket = thread::blockIdx_y() as usize;
        let first = chunk * PREFIX_ITEMS + local * 4;
        let base = bucket * blocks as usize;
        let mut values = [0u32; 4];
        for offset in 0..4 {
            if first + offset < blocks as usize {
                values[offset] = unsafe { histogram.add(base + first + offset).read() };
            }
        }
        let total = values[0]
            .wrapping_add(values[1])
            .wrapping_add(values[2])
            .wrapping_add(values[3]);
        let inclusive = block_inclusive(total, shared);
        let mut prefix = inclusive.wrapping_sub(total);
        for offset in 0..4 {
            if first + offset < blocks as usize {
                unsafe { histogram.add(base + first + offset).write(prefix) };
            }
            prefix = prefix.wrapping_add(values[offset]);
        }
        if local == THREADS - 1 {
            unsafe { sums.add(bucket * chunks as usize + chunk).write(inclusive) };
        }
    }

    #[kernel]
    pub fn radix_prefix_totals(sums: *mut u32, chunks: u32, bins: *mut u32) {
        static mut TOTALS: SharedArray<u32, WARPS> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut TOTALS) };
        let local = thread::threadIdx_x() as usize;
        let bucket = thread::blockIdx_x() as usize;
        let mut start = 0usize;
        let mut carry = 0u32;
        while start < chunks as usize {
            let position = start + local;
            let value = if position < chunks as usize {
                unsafe { sums.add(bucket * chunks as usize + position).read() }
            } else {
                0
            };
            let inclusive = block_inclusive(value, shared);
            if position < chunks as usize {
                unsafe {
                    sums.add(bucket * chunks as usize + position)
                        .write(carry.wrapping_add(inclusive.wrapping_sub(value)))
                };
            }
            carry = carry.wrapping_add(unsafe { shared.add(WARPS - 1).read() });
            thread::sync_threads();
            start += THREADS;
        }
        if local == 0 {
            unsafe { bins.add(bucket).write(carry) };
        }
    }

    #[kernel]
    pub fn radix_prefix_bins(bins: *mut u32) {
        static mut TOTALS: SharedArray<u32, WARPS> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut TOTALS) };
        let local = thread::threadIdx_x() as usize;
        let value = unsafe { bins.add(local).read() };
        let inclusive = block_inclusive(value, shared);
        unsafe { bins.add(local).write(inclusive.wrapping_sub(value)) };
    }

    #[kernel]
    pub fn radix_scatter(
        input: *const [u32; 4],
        output: *mut [u32; 4],
        count: u32,
        blocks: u32,
        chunks: u32,
        shift: u32,
        final_table: u32,
        histogram: *const u32,
        sums: *const u32,
        bins: *const u32,
        errors: *mut u32,
    ) {
        static mut HISTOGRAM: SharedArray<u32, { WARPS * BINS }> = SharedArray::UNINIT;
        static mut RANKS: SharedArray<u32, ITEMS> = SharedArray::UNINIT;
        static mut BASES: SharedArray<u32, BINS> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut HISTOGRAM) };
        let ranks = unsafe { SharedArray::as_raw_mut_ptr(&raw mut RANKS) };
        let bases = unsafe { SharedArray::as_raw_mut_ptr(&raw mut BASES) };
        let local = thread::threadIdx_x() as usize;
        let warp_index = local >> 5;
        let lane = local & 31;
        let block = thread::blockIdx_x() as usize;
        let chunk = block / PREFIX_ITEMS;
        for offset in 0..WARPS {
            unsafe { shared.add(offset * BINS + local).write(0) };
        }
        let base = unsafe {
            histogram
                .add(local * blocks as usize + block)
                .read()
                .wrapping_add(sums.add(local * chunks as usize + chunk).read())
                .wrapping_add(bins.add(local).read())
        };
        unsafe { bases.add(local).write(base) };
        thread::sync_threads();
        for iteration in 0..8 {
            let local_position = warp_index * 256 + iteration * 32 + lane;
            let position = block * ITEMS + local_position;
            let valid = position < count as usize;
            let entry = if valid {
                unsafe { input.add(position).read() }
            } else {
                [0; 4]
            };
            let bucket = digit(entry, shift, final_table);
            let active = warp::ballot_sync(u32::MAX, valid);
            let matching = warp::match_any_sync(u32::MAX, bucket as u32) & active;
            let leader = matching.trailing_zeros().min(31);
            let mut preceding = 0;
            if valid && lane as u32 == leader {
                let counter =
                    unsafe { BlockAtomicU32::from_ptr(shared.add(warp_index * BINS + bucket)) };
                preceding = counter.fetch_add(matching.count_ones(), AtomicOrdering::Relaxed);
            }
            preceding = warp::shuffle_sync(u32::MAX, preceding, leader);
            if valid {
                let rank = preceding.wrapping_add((matching & warp::lanemask_lt()).count_ones());
                unsafe { ranks.add(local_position).write(rank) };
            }
        }
        thread::sync_threads();
        let mut preceding = 0u32;
        for warp_index in 0..WARPS {
            let address = unsafe { shared.add(warp_index * BINS + local) };
            let count = unsafe { address.read() };
            unsafe { address.write(preceding) };
            preceding = preceding.wrapping_add(count);
        }
        thread::sync_threads();
        for iteration in 0..8 {
            let local_position = warp_index * 256 + iteration * 32 + lane;
            let position = block * ITEMS + local_position;
            if position < count as usize {
                let entry = unsafe { input.add(position).read() };
                let bucket = digit(entry, shift, final_table);
                let destination = unsafe {
                    ranks
                        .add(local_position)
                        .read()
                        .wrapping_add(shared.add(warp_index * BINS + bucket).read())
                        .wrapping_add(bases.add(bucket).read())
                };
                if destination < count {
                    unsafe { output.add(destination as usize).write(entry) };
                } else {
                    unsafe { DeviceAtomicU32::from_ptr(errors) }
                        .fetch_or(2, AtomicOrdering::Relaxed);
                }
            }
        }
    }

    #[kernel]
    pub fn radix_scatter_coalesced(
        input: *const [u32; 4],
        output: *mut [u32; 4],
        count: u32,
        blocks: u32,
        chunks: u32,
        shift: u32,
        final_table: u32,
        histogram: *const u32,
        sums: *const u32,
        bins: *const u32,
        errors: *mut u32,
    ) {
        static mut HISTOGRAM: SharedArray<u32, { WARPS * BINS }> = SharedArray::UNINIT;
        static mut RANKS: SharedArray<u16, ITEMS> = SharedArray::UNINIT;
        static mut BASES: SharedArray<u32, BINS> = SharedArray::UNINIT;
        static mut LOCAL_BASES: SharedArray<u32, BINS> = SharedArray::UNINIT;
        static mut TOTALS: SharedArray<u32, WARPS> = SharedArray::UNINIT;
        static mut SORTED: SharedArray<[u32; 4], ITEMS> = SharedArray::UNINIT;
        let shared = unsafe { SharedArray::as_raw_mut_ptr(&raw mut HISTOGRAM) };
        let ranks = unsafe { SharedArray::as_raw_mut_ptr(&raw mut RANKS) };
        let bases = unsafe { SharedArray::as_raw_mut_ptr(&raw mut BASES) };
        let local_bases = unsafe { SharedArray::as_raw_mut_ptr(&raw mut LOCAL_BASES) };
        let totals = unsafe { SharedArray::as_raw_mut_ptr(&raw mut TOTALS) };
        let sorted = unsafe { SharedArray::as_raw_mut_ptr(&raw mut SORTED) };
        let local = thread::threadIdx_x() as usize;
        let warp_index = local >> 5;
        let lane = local & 31;
        let block = thread::blockIdx_x() as usize;
        let chunk = block / PREFIX_ITEMS;
        let tile_count = (count as usize - block * ITEMS).min(ITEMS);
        for offset in 0..WARPS {
            unsafe { shared.add(offset * BINS + local).write(0) };
        }
        let base = unsafe {
            histogram
                .add(local * blocks as usize + block)
                .read()
                .wrapping_add(sums.add(local * chunks as usize + chunk).read())
                .wrapping_add(bins.add(local).read())
        };
        unsafe { bases.add(local).write(base) };
        thread::sync_threads();
        for iteration in 0..8 {
            let local_position = warp_index * 256 + iteration * 32 + lane;
            let position = block * ITEMS + local_position;
            let valid = local_position < tile_count;
            let entry = if valid {
                unsafe { input.add(position).read() }
            } else {
                [0; 4]
            };
            let bucket = digit(entry, shift, final_table);
            let active = warp::ballot_sync(u32::MAX, valid);
            let matching = warp::match_any_sync(u32::MAX, bucket as u32) & active;
            let leader = matching.trailing_zeros().min(31);
            let mut preceding = 0;
            if valid && lane as u32 == leader {
                let counter =
                    unsafe { BlockAtomicU32::from_ptr(shared.add(warp_index * BINS + bucket)) };
                preceding = counter.fetch_add(matching.count_ones(), AtomicOrdering::Relaxed);
            }
            preceding = warp::shuffle_sync(u32::MAX, preceding, leader);
            if valid {
                let rank = preceding.wrapping_add((matching & warp::lanemask_lt()).count_ones());
                unsafe { ranks.add(local_position).write(rank as u16) };
            }
        }
        thread::sync_threads();
        let mut preceding = 0u32;
        for warp_index in 0..WARPS {
            let address = unsafe { shared.add(warp_index * BINS + local) };
            let count = unsafe { address.read() };
            unsafe { address.write(preceding) };
            preceding = preceding.wrapping_add(count);
        }
        let inclusive = block_inclusive(preceding, totals);
        unsafe {
            local_bases
                .add(local)
                .write(inclusive.wrapping_sub(preceding))
        };
        thread::sync_threads();
        for iteration in 0..8 {
            let local_position = warp_index * 256 + iteration * 32 + lane;
            if local_position < tile_count {
                let entry = unsafe { input.add(block * ITEMS + local_position).read() };
                let bucket = digit(entry, shift, final_table);
                let destination = unsafe {
                    u32::from(ranks.add(local_position).read())
                        .wrapping_add(shared.add(warp_index * BINS + bucket).read())
                        .wrapping_add(local_bases.add(bucket).read())
                };
                if (destination as usize) < tile_count {
                    unsafe { sorted.add(destination as usize).write(entry) };
                } else {
                    unsafe { DeviceAtomicU32::from_ptr(errors) }
                        .fetch_or(4, AtomicOrdering::Relaxed);
                }
            }
        }
        thread::sync_threads();
        for iteration in 0..8 {
            let position = iteration * THREADS + local;
            if position < tile_count {
                let entry = unsafe { sorted.add(position).read() };
                let bucket = digit(entry, shift, final_table);
                let destination = unsafe {
                    bases
                        .add(bucket)
                        .read()
                        .wrapping_add(position as u32)
                        .wrapping_sub(local_bases.add(bucket).read())
                };
                if destination < count {
                    unsafe { output.add(destination as usize).write(entry) };
                } else {
                    unsafe { DeviceAtomicU32::from_ptr(errors) }
                        .fetch_or(2, AtomicOrdering::Relaxed);
                }
            }
        }
    }
}
