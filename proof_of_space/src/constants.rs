use once_cell::sync::Lazy;
use std::ops::IndexMut;

// Unique plot id which will be used as a ChaCha8 key, and determines the PoSpace.
pub const K_ID_LEN: u32 = 32;

// Distance between matching entries is stored in the offset
pub const K_OFFSET_SIZE: u32 = 10;

// Max matches a single entry can have, used for hardcoded memory allocation
pub const K_MAX_MATCHES_SINGLE_ENTRY: u32 = 30;
pub const K_MIN_BUCKETS: u32 = 16;
pub const K_MAX_BUCKETS: u32 = 128;

// During backprop and compress, the write pointer is ahead of the read pointer
// Note that the large the offset, the higher these values must be
pub const K_READ_MINUS_WRITE: u32 = 1 << K_OFFSET_SIZE;
pub const K_CACHED_POSITIONS_SIZE: u32 = K_READ_MINUS_WRITE * 4;

// Must be set high enough to prevent attacks of fast plotting
pub const K_MIN_PLOT_SIZE: u32 = 18;

// Set to 50 since k + kExtraBits + k*4 must not exceed 256 (BLAKE3 output size)
pub const K_MAX_PLOT_SIZE: u32 = 50;

// The amount of spare space used for sort on disk (multiplied time memory buffer size)
pub const K_SPARE_MULTIPLIER: u32 = 5;

// The proportion of memory to allocate to the Sort Manager for reading in buckets and sorting them
// The lower this number, the more memory must be provided by the caller. However, lowering the
// number also allows a higher proportion for writing, which reduces seeks for HDD.
pub const K_MEM_SORT_PROPORTION: f64 = 0.75;
pub const K_MEM_SORT_PROPORTION_LINE_POINT: f64 = 0.85;

// How many f7s per C1 entry, and how many C1 entries per C2 entry
pub const K_CHECKPOINT1INTERVAL: u32 = 10000;
pub const K_CHECKPOINT2INTERVAL: u32 = 10000;

// F1 evaluations are done in batches of 2^K_BATCH_SIZES
pub const K_BATCH_SIZES: u32 = 8;

// EPP for the final file, the higher this is, the less variability, and lower delta
// Note: if this is increased, ParkVector size must increase
pub const K_ENTRIES_PER_PARK: u32 = 2048;

// To store deltas for EPP entries, the average delta must be less than this number of bits
pub const K_MAX_AVERAGE_DELTA_TABLE1: f64 = 5.6;
pub const K_MAX_AVERAGE_DELTA: f64 = 3.5;

// C3 entries contain deltas for f7 values, the max average size is the following
pub const K_C3BITS_PER_ENTRY: f64 = 2.4;

// The number of bits in the stub is k minus this value
pub const K_STUB_MINUS_BITS: u8 = 3;

// The ANS encoding R values for the 7 final plot tables
// Tweaking the R values might allow lowering of the max average deltas, and reducing final
// plot size
pub const K_RVALUES: [f64; 7] = [4.7, 2.75, 2.75, 2.7, 2.6, 2.45, 0.0];

// The ANS encoding R value for the C3 checkpoint table
pub const K_C3R: f64 = 1.0;

// Plot format (no compatibility guarantees with other formats). If any of the
// above contants are changed, or file format is changed, the version should
// be incremented.
pub const K_FORMAT_DESCRIPTION: &str = "v1.0";

/// Div With modifications to affect rounding
///
/// # Examples
///
/// ```
/// let result = dg_xch_pos::constants::cdiv(11, 2);
/// assert_eq!(result, 6);
/// ```
///
/// ```
/// let result = dg_xch_pos::constants::cdiv(10, 2);
/// assert_eq!(result, 5);
/// ```
///
/// ```
/// let result = dg_xch_pos::constants::cdiv(9, 2);
/// assert_eq!(result, 5);
/// ```
///
/// # Panics
///
/// The function panics if the second argument is zero.
///
/// ```rust,should_panic
/// // panics on division by zero
/// dg_xch_pos::constants::cdiv(10, 0);
/// ```
#[must_use]
pub const fn cdiv(a: i32, b: i32) -> i32 {
    (a + b - 1) / b
}
#[must_use]
pub const fn ucdiv(a: u32, b: u32) -> u32 {
    (a + b - 1) / b
}
#[must_use]
pub const fn ucdiv64(a: u64, b: u64) -> u64 {
    (a + b - 1) / b
}
#[must_use]
pub const fn ucdiv_t(a: usize, b: usize) -> usize {
    (a + b - 1) / b
}
#[must_use]
pub const fn byte_align(num_bits: u32) -> u32 {
    num_bits + (8 - ((num_bits) % 8)) % 8
}
pub const BITS_PER_INTERVAL: u32 = 24000; //K_C3BITS_PER_ENTRY * K_CHECKPOINT1INTERVAL as f64 no const float math
                                          // ChaCha8 block size
pub const K_F1_BLOCK_SIZE_BITS: u16 = 512;
// ChaCha8 block size
pub const K_F1_BLOCK_SIZE: u16 = K_F1_BLOCK_SIZE_BITS / 8;

// Extra bits of output from the f functions. Instead of being a function from k -> k bits,
// it's a function from k -> k + kExtraBits bits. This allows less collisions in matches.
// Refer to the paper for mathematical motivations.
pub const K_EXTRA_BITS: u8 = 6;

// Convenience variable
pub const K_EXTRA_BITS_POW: u8 = 1 << K_EXTRA_BITS;

// B and C groups which constitute a bucket, or BC group. These groups determine how
// elements match with each other. Two elements must be in adjacent buckets to match.
pub const K_B: usize = 119;
pub const K_C: usize = 127;
pub const K_BC: usize = K_B * K_C;

pub const FSE_MAX_SYMBOL_VALUE: u32 = 255;
pub const K_VECTOR_LENS: [u8; 8] = [0, 0, 1, 2, 4, 4, 3, 2];

pub const VERSION: u16 = 1;

pub const HEADER_MAGIC: [u8; 19] = [
    0x50, 0x72, 0x6f, 0x6f, 0x66, 0x20, 0x6f, 0x66, 0x20, 0x53, 0x70, 0x61, 0x63, 0x65, 0x20, 0x50,
    0x6c, 0x6f, 0x74,
]; //

pub const HEADER_V2_MAGIC: [u8; 4] = [0x50, 0x4c, 0x4f, 0x54];

pub struct PlotEntry {
    pub y: u64,
    pub pos: u64,
    pub offset: u64,
    pub left_metadata: u128, // We only use left_metadata, unless metadata does not
    pub right_metadata: u128, // fit in 128 bits.
    pub used: bool,          // Whether the entry was used in the next table of matches
    pub read_posoffset: u64, // The combined pos and offset that this entry points to
}
pub static L_TARGETS: Lazy<Vec<Vec<Vec<u16>>>> = Lazy::new(gen_l_targets);

#[allow(clippy::cast_possible_truncation)]
fn gen_l_targets() -> Vec<Vec<Vec<u16>>> {
    let mut targets = vec![vec![vec![0u16; K_EXTRA_BITS_POW as usize]; K_BC]; 2];
    let mut parity = 0;
    while parity < 2 {
        let mut i: usize = 0;
        while i < K_BC {
            let j = i / K_C;
            let mut m = 0;
            while m < u16::from(K_EXTRA_BITS_POW) {
                *targets
                    .index_mut(parity as usize)
                    .index_mut(i)
                    .index_mut(m as usize) = ((j as u16 + m) % K_B as u16) * K_C as u16
                    + (((2 * m + parity) * (2 * m + parity) + i as u16) % K_C as u16);
                m += 1;
            }
            i += 1;
        }
        parity += 1;
    }
    targets
}
