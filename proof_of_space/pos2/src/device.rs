#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub meta: u64,
    pub fragment: u64,
    pub info: u32,
    pub x_bits: u32,
    pub xs: [u32; 8],
    pub valid: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Config {
    pub plot_id: [u8; 32],
    pub k: u32,
    pub strength: u32,
    pub testnet: u32,
}

const fn multiply(mut left: u8, mut right: u8) -> u8 {
    let mut output = 0;
    let mut bit = 0;
    while bit < 8 {
        if right & 1 != 0 {
            output ^= left;
        }
        left = (left << 1) ^ if left & 128 != 0 { 27 } else { 0 };
        right >>= 1;
        bit += 1;
    }
    output
}

const fn substitutions() -> [u8; 256] {
    let mut output = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut inverse = 1u8;
        let mut base = index as u8;
        let mut exponent = 254;
        while exponent > 0 {
            if exponent & 1 != 0 {
                inverse = multiply(inverse, base);
            }
            base = multiply(base, base);
            exponent >>= 1;
        }
        output[index] = inverse
            ^ inverse.rotate_left(1)
            ^ inverse.rotate_left(2)
            ^ inverse.rotate_left(3)
            ^ inverse.rotate_left(4)
            ^ 0x63;
        index += 1;
    }
    output
}

const SBOX: [u8; 256] = substitutions();

const fn aes_table() -> [u32; 256] {
    let mut output = [0; 256];
    let mut index = 0;
    while index < 256 {
        let value = SBOX[index] as u32;
        let doubled = ((value << 1) ^ if value & 128 != 0 { 27 } else { 0 }) & 255;
        output[index] = doubled | (value << 8) | (value << 16) | ((value ^ doubled) << 24);
        index += 1;
    }
    output
}

pub const AES_TABLE: [u32; 256] = aes_table();

#[inline]
fn aes_column(table: &[u32; 256], first: u32, second: u32, third: u32, fourth: u32) -> u32 {
    table[(first & 255) as usize]
        ^ table[((second >> 8) & 255) as usize].rotate_left(8)
        ^ table[((third >> 16) & 255) as usize].rotate_left(16)
        ^ table[(fourth >> 24) as usize].rotate_left(24)
}

#[inline]
fn aes_round(state: [u32; 4], key: [u32; 4], table: &[u32; 256]) -> [u32; 4] {
    [
        aes_column(table, state[0], state[1], state[2], state[3]) ^ key[0],
        aes_column(table, state[1], state[2], state[3], state[0]) ^ key[1],
        aes_column(table, state[2], state[3], state[0], state[1]) ^ key[2],
        aes_column(table, state[3], state[0], state[1], state[2]) ^ key[3],
    ]
}

fn key_word(config: Config, offset: usize) -> u32 {
    u32::from_le_bytes([
        config.plot_id[offset],
        config.plot_id[offset + 1],
        config.plot_id[offset + 2],
        config.plot_id[offset + 3],
    ])
}

pub fn hash(config: Config, words: [u32; 4], rounds: u32) -> [u32; 4] {
    let first_key = [
        key_word(config, 0),
        key_word(config, 4),
        key_word(config, 8),
        key_word(config, 12),
    ];
    let second_key = [
        key_word(config, 16),
        key_word(config, 20),
        key_word(config, 24),
        key_word(config, 28),
    ];
    hash_with_keys([first_key, second_key], words, rounds, &AES_TABLE)
}

#[inline]
pub fn hash_with_keys(
    keys: [[u32; 4]; 2],
    words: [u32; 4],
    rounds: u32,
    table: &[u32; 256],
) -> [u32; 4] {
    let mut state = words;
    for _ in 0..rounds {
        state = aes_round(state, keys[0], table);
        state = aes_round(state, keys[1], table);
    }
    state
}

fn mask(bits: u32) -> u64 {
    u64::MAX >> (64 - bits)
}

pub fn generate(config: Config, value: u32) -> Record {
    let input = if config.testnet == 0 {
        value
    } else {
        value ^ 0xA3B1C4D7
    };
    generate_from_hash(config, value, hash(config, [input, 0, 0, 0], 16))
}

#[inline]
pub fn generate_from_hash(config: Config, value: u32, lanes: [u32; 4]) -> Record {
    let mut record = Record {
        meta: u64::from(value),
        info: lanes[0] & mask(config.k) as u32,
        fragment: 0,
        x_bits: 0,
        xs: [0; 8],
        valid: 1,
        reserved: 0,
    };
    record.xs[0] = value;
    record
}

pub fn target(config: Config, table: u32, left: Record, key: u32) -> u32 {
    let rounds = if table == 1 {
        16 << (config.strength - 2)
    } else {
        16
    };
    let value = hash(
        config,
        [table, key, left.meta as u32, (left.meta >> 32) as u32],
        rounds,
    )[0];
    target_from_hash(config, table, left, key, value)
}

#[inline]
pub fn target_from_hash(config: Config, table: u32, left: Record, key: u32, value: u32) -> u32 {
    let sections = if config.k < 28 { 2 } else { config.k - 26 };
    let count = 1u32 << sections;
    let section = left.info >> (config.k - sections);
    let rotated = (section << 1) | (section >> (sections - 1));
    let bumped = (rotated + 1) & (count - 1);
    let partner = ((bumped >> 1) | (bumped << (sections - 1))) & (count - 1);
    let key_bits = if table == 1 { 2 } else { config.strength };
    let target_bits = config.k - sections - key_bits;
    (partner << (config.k - sections)) | (key << target_bits) | (value & mask(target_bits) as u32)
}

fn rotate(value: u32, shift: u32, bits: u32) -> u32 {
    ((value << shift) & mask(bits) as u32) | (value >> (bits - shift))
}

#[inline]
fn fragment_round_key(config: Config, round: u32) -> [u32; 4] {
    let start = round * (256 - 3 * config.k) / 3;
    let offset = start % 8;
    let bytes = (offset + 3 * config.k).div_ceil(8);
    let mut segment = 0u64;
    for index in 0..bytes {
        segment = (segment << 8) | u64::from(config.plot_id[(start / 8 + index) as usize]);
    }
    let key = (segment >> (bytes * 8 - offset - 3 * config.k))
        & if config.k * 3 >= 64 {
            u64::MAX
        } else {
            mask(config.k * 3)
        };
    [
        (key & mask(config.k)) as u32,
        (key.wrapping_shr(config.k) & mask(config.k)) as u32,
        (key.wrapping_shr(2 * config.k) & mask(config.k)) as u32,
        0,
    ]
}

#[inline]
pub fn fragment_round_keys(config: Config) -> [[u32; 4]; 4] {
    [
        fragment_round_key(config, 0),
        fragment_round_key(config, 1),
        fragment_round_key(config, 2),
        fragment_round_key(config, 3),
    ]
}

#[inline]
pub fn fragment_round_halves(bits: u32, left: u32, right: u32, key: [u32; 4]) -> (u32, u32) {
    let bitmask = mask(bits) as u32;
    let mut first = right;
    let mut second = key[0];
    let mut third = key[1];
    let mut fourth = key[2];
    first = first.wrapping_add(second) & bitmask;
    fourth = rotate(fourth ^ first, 16, bits);
    third = third.wrapping_add(fourth) & bitmask;
    second = rotate(second ^ third, 12, bits);
    first = first.wrapping_add(second) & bitmask;
    fourth = rotate(fourth ^ first, 8, bits);
    third = third.wrapping_add(fourth) & bitmask;
    second = rotate(second ^ third, 7, bits);
    (right, (left ^ second) & bitmask)
}

#[inline]
pub fn fragment(config: Config, input: u64) -> u64 {
    let mut left = (input >> config.k) as u32;
    let mut right = (input & mask(config.k)) as u32;
    for round in 0..4 {
        (left, right) =
            fragment_round_halves(config.k, left, right, fragment_round_key(config, round));
    }
    (u64::from(left) << config.k) | u64::from(right)
}

pub fn pair(config: Config, table: u32, left: Record, right: Record) -> Record {
    let rounds = if table == 1 {
        16 << (config.strength - 2)
    } else {
        16
    };
    let lanes = hash(
        config,
        [
            left.meta as u32,
            (left.meta >> 32) as u32,
            right.meta as u32,
            (right.meta >> 32) as u32,
        ],
        rounds,
    );
    pair_from_hash(config, table, left, right, lanes)
}

#[inline]
pub fn pair_from_hash(
    config: Config,
    table: u32,
    left: Record,
    right: Record,
    lanes: [u32; 4],
) -> Record {
    let mut result = pair_fields_from_hash(config, table, left, right, lanes);
    if result.valid != 0 {
        let width = 1usize << (table - 1);
        for index in 0..width {
            result.xs[index] = left.xs[index];
            result.xs[index + width] = right.xs[index];
        }
    }
    result
}

#[inline]
pub fn pair_fields_from_hash(
    config: Config,
    table: u32,
    left: Record,
    right: Record,
    lanes: [u32; 4],
) -> Record {
    let test_bits = if table == 1 { 2 } else { config.strength };
    let mut result = Record {
        meta: 0,
        fragment: 0,
        info: lanes[0] & mask(config.k) as u32,
        x_bits: 0,
        xs: [0; 8],
        valid: 0,
        reserved: 0,
    };
    if lanes[3] & mask(test_bits) as u32 != 0 {
        return result;
    }
    result.valid = 1;
    if table == 1 {
        result.meta = (left.meta << config.k) | right.meta;
    } else {
        result.meta = (u64::from(lanes[1]) | (u64::from(lanes[2]) << 32)) & mask(2 * config.k);
        result.x_bits = (((left.meta >> config.k) >> (config.k / 2)) << (config.k / 2)) as u32
            | ((right.meta >> config.k) >> (config.k / 2)) as u32;
    }
    if table == 3 {
        result.fragment = fragment(
            config,
            (u64::from(left.x_bits) << config.k) | u64::from(right.x_bits),
        );
    }
    result
}

pub fn less(left: Record, right: Record, final_table: bool) -> bool {
    if left.valid != right.valid {
        return left.valid > right.valid;
    }
    if final_table && left.fragment != right.fragment {
        return left.fragment < right.fragment;
    }
    if !final_table && left.info != right.info {
        return left.info < right.info;
    }
    if !final_table && left.meta != right.meta {
        return left.meta < right.meta;
    }
    for index in 0..8 {
        if left.xs[index] != right.xs[index] {
            return left.xs[index] < right.xs[index];
        }
    }
    false
}

pub fn scan(
    config: Config,
    table: u32,
    entries: &[Record],
    length: usize,
    left_index: usize,
    wanted: u32,
    max_work: u32,
) -> (Record, u32) {
    let left = entries[left_index];
    let keys = 1u32 << if table == 1 { 2 } else { config.strength };
    let mut count = 0u32;
    let mut work = 0u32;
    let empty = Record {
        meta: 0,
        fragment: 0,
        info: 0,
        x_bits: 0,
        xs: [0; 8],
        valid: 0,
        reserved: 0,
    };
    for key in 0..keys {
        if work == max_work {
            return (empty, u32::MAX);
        }
        work += 1;
        let info = target(config, table, left, key);
        let mut lower = 0;
        let mut upper = length;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if entries[middle].info < info {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        while lower < length && entries[lower].info == info {
            if work == max_work {
                return (empty, u32::MAX);
            }
            work += 1;
            let candidate = pair(config, table, left, entries[lower]);
            if candidate.valid != 0 {
                if count == wanted {
                    return (candidate, count);
                }
                count += 1;
            }
            lower += 1;
        }
    }
    (empty, count)
}
