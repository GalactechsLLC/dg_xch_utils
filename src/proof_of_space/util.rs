use std::mem::size_of;

pub fn bytes_to_u64<T: AsRef<[u8]>>(bytes: T) -> u64 {
    const SIZE: usize = size_of::<u64>();
    let bytes = bytes.as_ref();
    let mut buf: [u8; SIZE] = [0; SIZE];
    if bytes.len() < SIZE {
        let mut bytes = bytes.to_vec();
        bytes.extend(vec![0; SIZE - bytes.len()]);
        buf.copy_from_slice(&bytes);
    } else {
        buf.copy_from_slice(&bytes[0..SIZE]);
    }
    u64::from_be_bytes(buf)
}

// 'bytes' points to a big-endian 64 bit value (possibly truncated, if
// (start_bit % 8 + num_bits > 64)). Returns the integer that starts at
// 'start_bit' that is 'num_bits' long (as a native-endian integer).
//
// Note: requires that 8 bytes after the first sliced byte are addressable
// (regardless of 'num_bits'). In practice it can be ensured by allocating
// extra 7 bytes to all memory buffers passed to this function.
pub fn slice_u64from_bytes<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u64 {
    let mut bytes = bytes.as_ref().to_vec();
    let mut start_bit = start_bit;
    if start_bit + num_bits > 64 {
        bytes.push((start_bit / 8) as u8);
        start_bit %= 8;
    }
    let mut tmp = bytes_to_u64(&bytes);
    tmp <<= start_bit;
    tmp >>= 64 - num_bits;
    tmp
}

pub fn slice_u64from_bytes_full<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u64 {
    let last_bit = start_bit + num_bits;
    let mut r = slice_u64from_bytes(bytes.as_ref(), start_bit, num_bits);
    if start_bit % 8 + num_bits > 64 {
        r |= bytes.as_ref()[(last_bit / 8) as usize] as u64 >> (8 - last_bit % 8);
    }
    r
}

pub fn slice_u128from_bytes<T: AsRef<[u8]>>(bytes: T, start_bit: u32, num_bits: u32) -> u128 {
    if num_bits <= 64 {
        slice_u64from_bytes_full(bytes, start_bit, num_bits) as u128
    } else {
        let num_bits_high = num_bits - 64;
        let high = slice_u64from_bytes_full(bytes.as_ref(), start_bit, num_bits_high);
        let low = slice_u64from_bytes_full(bytes.as_ref(), start_bit + num_bits_high, 64);
        ((high as u128) << 64) | low as u128
    }
}
