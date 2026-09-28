use crate::device::{self, Config, Record};
use cuda_device::atomic::{AtomicOrdering, BlockAtomicU32, DeviceAtomicU32};
use cuda_device::{SharedArray, cuda_module, kernel, thread};

const INFO_COUNT: u32 = 1 << 28;
const LOCAL_OUTPUT_CAPACITY: usize = 512;
const PAIR_CONFIG: Config = Config {
    plot_id: [0; 32],
    k: 28,
    strength: 2,
    testnet: 0,
};

#[inline]
fn hash_keys(configuration: &[u32; 32]) -> [[u32; 4]; 2] {
    [
        [
            configuration[0],
            configuration[1],
            configuration[2],
            configuration[3],
        ],
        [
            configuration[4],
            configuration[5],
            configuration[6],
            configuration[7],
        ],
    ]
}

#[inline(always)]
fn aes_column<const REPLICAS: usize>(
    table: &[[u32; REPLICAS]; 256],
    replica: usize,
    first: u32,
    second: u32,
    third: u32,
    fourth: u32,
) -> u32 {
    table[(first & 255) as usize][replica]
        ^ table[((second >> 8) & 255) as usize][replica].rotate_left(8)
        ^ table[((third >> 16) & 255) as usize][replica].rotate_left(16)
        ^ table[(fourth >> 24) as usize][replica].rotate_left(24)
}

#[inline(always)]
fn aes_round<const REPLICAS: usize>(
    state: [u32; 4],
    key: [u32; 4],
    table: &[[u32; REPLICAS]; 256],
    replica: usize,
) -> [u32; 4] {
    [
        aes_column(table, replica, state[0], state[1], state[2], state[3]) ^ key[0],
        aes_column(table, replica, state[1], state[2], state[3], state[0]) ^ key[1],
        aes_column(table, replica, state[2], state[3], state[0], state[1]) ^ key[2],
        aes_column(table, replica, state[3], state[0], state[1], state[2]) ^ key[3],
    ]
}

#[inline(always)]
fn resident_hash<const REPLICAS: usize>(
    keys: [[u32; 4]; 2],
    words: [u32; 4],
    table: &[[u32; REPLICAS]; 256],
    replica: usize,
) -> [u32; 4] {
    let mut state = words;
    for _ in 0..16 {
        state = aes_round(state, keys[0], table, replica);
        state = aes_round(state, keys[1], table, replica);
    }
    state
}

#[inline]
fn record(entry: [u32; 4]) -> Record {
    Record {
        meta: u64::from(entry[0]) | (u64::from(entry[1]) << 32),
        fragment: 0,
        info: entry[2],
        x_bits: entry[3],
        xs: [0; 8],
        valid: 1,
        reserved: 0,
    }
}

#[inline]
fn fragment(configuration: &[u32; 32], left: u32, right: u32) -> u64 {
    let (left, right) = device::fragment_round_halves(
        28,
        left,
        right,
        [configuration[8], configuration[9], configuration[10], 0],
    );
    let (left, right) = device::fragment_round_halves(
        28,
        left,
        right,
        [configuration[12], configuration[13], configuration[14], 0],
    );
    let (left, right) = device::fragment_round_halves(
        28,
        left,
        right,
        [configuration[16], configuration[17], configuration[18], 0],
    );
    let (left, right) = device::fragment_round_halves(
        28,
        left,
        right,
        [configuration[20], configuration[21], configuration[22], 0],
    );
    (u64::from(left) << 28) | u64::from(right)
}

#[inline]
fn reserve(
    counter: &DeviceAtomicU32,
    amount: u32,
    limit: u32,
    errors: &DeviceAtomicU32,
    flag: u32,
) -> Option<u32> {
    if amount == 0 {
        return Some(0);
    }
    let mut observed = counter.load(AtomicOrdering::Relaxed);
    loop {
        if observed > limit || amount > limit - observed {
            errors.fetch_or(flag, AtomicOrdering::Relaxed);
            return None;
        }
        match counter.compare_exchange(
            observed,
            observed + amount,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return Some(observed),
            Err(current) => observed = current,
        }
    }
}

#[cuda_module]
pub mod kernels {
    use super::*;

    #[kernel]
    pub fn resident_extract_fragments(
        input: &[[u32; 4]],
        start: u32,
        count: u32,
        output: *mut u64,
        counters: *mut u32,
    ) {
        let offset = thread::index_1d().get();
        if offset >= count as usize {
            return;
        }
        let position = start as usize + offset;
        if position >= input.len() {
            let errors = unsafe { DeviceAtomicU32::from_ptr(counters.add(2)) };
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let entry = input[position];
        unsafe {
            output
                .add(offset)
                .write(u64::from(entry[0]) | (u64::from(entry[1]) << 32));
        }
    }

    #[kernel]
    pub fn resident_generate(configuration: [u32; 32], output: *mut [u32; 4], counters: *mut u32) {
        static mut AES: SharedArray<u32, 256> = SharedArray::UNINIT;
        let shared_aes = unsafe { SharedArray::as_raw_mut_ptr(&raw mut AES) };
        let mut table_index = thread::threadIdx_x() as usize;
        while table_index < 256 {
            unsafe {
                shared_aes
                    .add(table_index)
                    .write(device::AES_TABLE[table_index]);
            }
            table_index += thread::blockDim_x() as usize;
        }
        thread::sync_threads();
        let offset = thread::index_1d().get();
        if offset >= configuration[27] as usize {
            return;
        }
        let errors = unsafe { DeviceAtomicU32::from_ptr(counters.add(2)) };
        if offset >= configuration[29] as usize {
            errors.fetch_or(1, AtomicOrdering::Relaxed);
            return;
        }
        let position = configuration[26].wrapping_add(offset as u32);
        if position < configuration[26] || position >= INFO_COUNT {
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let input = position
            ^ if configuration[25] != 0 {
                0xa3b1c4d7
            } else {
                0
            };
        let table = unsafe { &*shared_aes.cast::<[u32; 256]>() };
        let lanes = device::hash_with_keys(hash_keys(&configuration), [input, 0, 0, 0], 16, table);
        let generated = device::generate_from_hash(PAIR_CONFIG, position, lanes);
        unsafe {
            output.add(offset).write([position, 0, generated.info, 0]);
        }
    }

    #[kernel]
    pub fn resident_build_index(
        configuration: [u32; 32],
        input: &[[u32; 4]],
        index: *mut u32,
        counters: *mut u32,
    ) {
        let offset = thread::index_1d().get();
        if offset >= configuration[27] as usize {
            return;
        }
        let errors = unsafe { DeviceAtomicU32::from_ptr(counters.add(2)) };
        let position = configuration[26].wrapping_add(offset as u32);
        if position < configuration[26]
            || position >= configuration[28]
            || position as usize >= input.len()
        {
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let info = input[position as usize][2];
        if info >= INFO_COUNT {
            errors.fetch_or(4, AtomicOrdering::Relaxed);
            return;
        }
        let mut lower = 0;
        if position != 0 {
            let previous = input[position as usize - 1][2];
            if previous > info {
                errors.fetch_or(4, AtomicOrdering::Relaxed);
                return;
            }
            lower = previous + 1;
        }
        if lower <= info {
            if info - lower >= 4096 {
                errors.fetch_or(4, AtomicOrdering::Relaxed);
                return;
            }
            for value in lower..=info {
                unsafe {
                    index.add(value as usize).write(position);
                }
            }
        }
        if position + 1 == configuration[28] {
            if INFO_COUNT - info > 4096 {
                errors.fetch_or(4, AtomicOrdering::Relaxed);
                return;
            }
            for value in info + 1..=INFO_COUNT {
                unsafe {
                    index.add(value as usize).write(configuration[28]);
                }
            }
        }
    }

    #[kernel]
    pub fn resident_match_table(
        configuration: [u32; 32],
        input: &[[u32; 4]],
        index: &[u32],
        output: *mut [u32; 4],
        counters: *mut u32,
    ) {
        static mut AES: SharedArray<[u32; 1], 256> = SharedArray::UNINIT;
        static mut OUTPUT: SharedArray<[u32; 4], LOCAL_OUTPUT_CAPACITY> = SharedArray::UNINIT;
        static mut STATE: SharedArray<u32, 5> = SharedArray::UNINIT;
        let shared_aes = unsafe { SharedArray::as_raw_mut_ptr(&raw mut AES) };
        let shared_output = unsafe { SharedArray::as_raw_mut_ptr(&raw mut OUTPUT) };
        let state = unsafe { SharedArray::as_raw_mut_ptr(&raw mut STATE) };
        match_table::<1>(
            configuration,
            input,
            index,
            output,
            counters,
            shared_aes,
            shared_output,
            state,
        );
    }

    #[kernel]
    pub fn resident_match_table_replicated(
        configuration: [u32; 32],
        input: &[[u32; 4]],
        index: &[u32],
        output: *mut [u32; 4],
        counters: *mut u32,
    ) {
        static mut AES: SharedArray<[u32; 8], 256> = SharedArray::UNINIT;
        static mut OUTPUT: SharedArray<[u32; 4], LOCAL_OUTPUT_CAPACITY> = SharedArray::UNINIT;
        static mut STATE: SharedArray<u32, 5> = SharedArray::UNINIT;
        let shared_aes = unsafe { SharedArray::as_raw_mut_ptr(&raw mut AES) };
        let shared_output = unsafe { SharedArray::as_raw_mut_ptr(&raw mut OUTPUT) };
        let state = unsafe { SharedArray::as_raw_mut_ptr(&raw mut STATE) };
        match_table::<8>(
            configuration,
            input,
            index,
            output,
            counters,
            shared_aes,
            shared_output,
            state,
        );
    }

    #[inline(always)]
    fn match_table<const REPLICAS: usize>(
        configuration: [u32; 32],
        input: &[[u32; 4]],
        index: &[u32],
        output: *mut [u32; 4],
        counters: *mut u32,
        shared_aes: *mut [u32; REPLICAS],
        shared_output: *mut [u32; 4],
        state: *mut u32,
    ) {
        let local = thread::threadIdx_x() as usize;
        let replica = local % REPLICAS;
        let mut table_index = local;
        while table_index < 256 {
            unsafe {
                shared_aes
                    .add(table_index)
                    .write([device::AES_TABLE[table_index]; REPLICAS]);
            }
            table_index += thread::blockDim_x() as usize;
        }
        if local == 0 {
            for offset in 0..5 {
                unsafe {
                    state.add(offset).write(0);
                }
            }
        }
        thread::sync_threads();
        let group_output_count = unsafe { BlockAtomicU32::from_ptr(state) };
        let group_pair_count = unsafe { BlockAtomicU32::from_ptr(state.add(1)) };
        let output_count = unsafe { DeviceAtomicU32::from_ptr(counters) };
        let pair_evaluations = unsafe { DeviceAtomicU32::from_ptr(counters.add(1)) };
        let errors = unsafe { DeviceAtomicU32::from_ptr(counters.add(2)) };
        let table = unsafe { &*shared_aes.cast::<[[u32; REPLICAS]; 256]>() };
        let keys = hash_keys(&configuration);
        let offset = thread::blockIdx_x() as usize * thread::blockDim_x() as usize + local;
        let position = configuration[26].wrapping_add(offset as u32);
        let is_active = offset < configuration[27] as usize;
        let number = configuration[24];
        let mut left = record([0; 4]);
        let mut ranges = [[0u32; 2]; 4];
        let mut match_infos = [0u32; 4];
        let mut pair_count = 0;
        if is_active {
            if position < configuration[26]
                || position >= configuration[28]
                || position as usize >= input.len()
                || number < 1
                || number > 3
                || index.len() <= INFO_COUNT as usize
            {
                errors.fetch_or(4, AtomicOrdering::Relaxed);
            } else {
                left = record(input[position as usize]);
                for target in 0..4 {
                    let key = target as u32;
                    let lanes = resident_hash(
                        keys,
                        [number, key, left.meta as u32, (left.meta >> 32) as u32],
                        table,
                        replica,
                    );
                    let wanted = device::target_from_hash(PAIR_CONFIG, number, left, key, lanes[0]);
                    let first = index[wanted as usize];
                    let last = index[wanted as usize + 1];
                    if first > last || last > configuration[28] || last as usize > input.len() {
                        errors.fetch_or(4, AtomicOrdering::Relaxed);
                    } else if last - first > 64 {
                        errors.fetch_or(8, AtomicOrdering::Relaxed);
                    } else {
                        ranges[target] = [first, last];
                        match_infos[target] = wanted;
                        pair_count += last - first;
                    }
                }
            }
        }
        group_pair_count.fetch_add(pair_count, AtomicOrdering::Relaxed);
        thread::sync_threads();
        if local == 0 {
            let amount = group_pair_count.load(AtomicOrdering::Relaxed);
            let allowed = errors.load(AtomicOrdering::Relaxed) == 0
                && reserve(pair_evaluations, amount, configuration[30], errors, 2).is_some();
            unsafe {
                state.add(2).write(u32::from(allowed));
            }
        }
        thread::sync_threads();
        if is_active && unsafe { state.add(2).read() } != 0 {
            for target in 0..4 {
                for candidate in ranges[target][0]..ranges[target][1] {
                    let right = record(input[candidate as usize]);
                    if right.info != match_infos[target] {
                        errors.fetch_or(4, AtomicOrdering::Relaxed);
                        continue;
                    }
                    let lanes = resident_hash(
                        keys,
                        [
                            left.meta as u32,
                            (left.meta >> 32) as u32,
                            right.meta as u32,
                            (right.meta >> 32) as u32,
                        ],
                        table,
                        replica,
                    );
                    let paired = device::pair_fields_from_hash(
                        PAIR_CONFIG,
                        if number == 3 { 2 } else { number },
                        left,
                        right,
                        lanes,
                    );
                    if paired.valid == 0 {
                        continue;
                    }
                    let meta = if number == 3 {
                        fragment(&configuration, left.x_bits, right.x_bits)
                    } else {
                        paired.meta
                    };
                    let value = [meta as u32, (meta >> 32) as u32, paired.info, paired.x_bits];
                    let location = group_output_count.fetch_add(1, AtomicOrdering::Relaxed);
                    if (location as usize) < LOCAL_OUTPUT_CAPACITY {
                        unsafe {
                            shared_output.add(location as usize).write(value);
                        }
                    } else if let Some(destination) =
                        reserve(output_count, 1, configuration[29], errors, 1)
                    {
                        unsafe {
                            output.add(destination as usize).write(value);
                        }
                    }
                }
            }
        }
        thread::sync_threads();
        if local == 0 {
            let length = group_output_count
                .load(AtomicOrdering::Relaxed)
                .min(LOCAL_OUTPUT_CAPACITY as u32);
            if let Some(destination) = reserve(output_count, length, configuration[29], errors, 1) {
                unsafe {
                    state.add(3).write(destination);
                    state.add(4).write(length);
                }
            }
        }
        thread::sync_threads();
        let start = unsafe { state.add(3).read() };
        let length = unsafe { state.add(4).read() } as usize;
        let mut position = local;
        while position < length {
            unsafe {
                output
                    .add(start as usize + position)
                    .write(shared_output.add(position).read());
            }
            position += thread::blockDim_x() as usize;
        }
    }
}
