use crate::constants::ucdiv_t;
use std::cmp::{max, min, Ordering};
use std::io::{Error, ErrorKind, Seek, SeekFrom};
use std::mem::size_of;
use std::ops;

#[derive(Debug, Default, Clone)]
pub struct BitReader {
    buffer: Vec<u64>,
    size: usize,
    position: usize,
    last_size: usize,
}
impl BitReader {
    #[must_use]
    pub fn new(value: u64, length: usize) -> Self {
        let mut s = Self::default();
        s.append_value(value, length);
        s
    }
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            size: 0,
            position: 0,
            last_size: 0,
        }
    }

    #[must_use]
    pub fn values(&self) -> Vec<u64> {
        let mut rtn = Vec::new();
        // Return if nothing to work on
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let mut i = 0;
        while i < self.buffer.len() - 1 {
            rtn.push(self.buffer[i]);
            i += 1;
        }
        rtn.push(self.buffer[i] << (64 - self.last_size));
        rtn
    }

    #[must_use]
    pub fn from_bytes_be(big_endian_bytes: &[u8], bit_size: usize) -> Self {
        if big_endian_bytes.is_empty() {
            Self::with_capacity(0)
        } else {
            let num_bytes = min(big_endian_bytes.len(), bit_size / 8);
            let mut extra_space = if num_bytes * 8 <= bit_size {
                bit_size - num_bytes * 8
            } else {
                0
            };
            let mut reader = Self::with_capacity(num_bytes / 8 + 1);
            while extra_space >= 64 {
                reader.append_value(0, 64);
                extra_space -= 64;
                reader.size += 64;
            }
            if extra_space > 0 {
                reader.append_value(0, extra_space);
                reader.size += extra_space;
            }
            let mut i = 0;
            while i < num_bytes {
                let mut val = 0u64;
                let mut bucket_size = 0;
                let mut j = i;
                while j < i + size_of::<u64>() && j < num_bytes {
                    val = (val << 8) + u64::from(big_endian_bytes[j]);
                    bucket_size += 8;
                    j += 1;
                }
                reader.append_value(val, bucket_size);
                i += size_of::<u64>();
            }
            reader
        }
    }
    #[allow(clippy::cast_possible_wrap)]
    #[must_use]
    pub fn from_bytes_be_offset(
        big_endian_bytes: &[u8],
        bit_size: usize,
        bit_offset: usize,
    ) -> Self {
        if big_endian_bytes.is_empty() {
            Self::with_capacity(0)
        } else {
            let mut bit_offset = bit_offset;
            let mut big_endian_bytes = big_endian_bytes;
            let start_field = bit_offset >> 6; // div 64
            let end_field = (start_field * 64 + bit_size) >> 6; // div 64
            let field_count = (end_field - start_field) + 1;
            let mut u64_buf = [0u8; size_of::<u64>()];
            big_endian_bytes = &big_endian_bytes[start_field * size_of::<u64>()..];
            let mut reader = Self::with_capacity(field_count);
            bit_offset -= start_field * 64;
            {
                u64_buf.fill(0);
                u64_buf[0..min(8, big_endian_bytes.len())]
                    .copy_from_slice(&big_endian_bytes[0..min(8, big_endian_bytes.len())]);
                let mut field = u64::from_be_bytes(u64_buf);
                let first_field_avail = 64 - bit_offset;
                let first_field_bits = min(first_field_avail, bit_size);
                let mask = 0xFFFF_FFFF_FFFF_FFFF >> (64 - first_field_bits);
                field = field >> (first_field_avail - first_field_bits) & mask;
                reader.append_value(field, first_field_bits);
                big_endian_bytes = &big_endian_bytes[size_of::<u64>()..];
            }
            // Write any full fields
            let full_field_count = max(0, field_count as isize - 2);
            for _ in 0..full_field_count {
                u64_buf.fill(0);
                u64_buf[0..min(8, big_endian_bytes.len())]
                    .copy_from_slice(&big_endian_bytes[0..min(8, big_endian_bytes.len())]);
                let field = u64::from_be_bytes(u64_buf);
                reader.append_value(field, 64);
                big_endian_bytes = &big_endian_bytes[size_of::<u64>()..];
            }
            // Write any partial final field
            if field_count > 1 {
                let last_field_bits = (bit_size + bit_offset) - (field_count - 1) * 64;
                u64_buf.fill(0);
                u64_buf[0..min(8, big_endian_bytes.len())]
                    .copy_from_slice(&big_endian_bytes[0..min(8, big_endian_bytes.len())]);
                let mut field = u64::from_be_bytes(u64_buf);
                field >>= 64 - last_field_bits;
                reader.append_value(field, last_field_bits);
            }
            reader
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn append_value(&mut self, value: u64, length: usize) {
        if self.buffer.is_empty() || self.last_size == 64 {
            self.buffer.push(value);
            self.last_size = length;
        } else {
            let free_bits = 64 - self.last_size;
            let len: usize = self.buffer.len() - 1;
            if self.last_size == 0 && length == 64 {
                self.buffer[len] = value;
                self.last_size = length;
            } else if length <= free_bits {
                self.buffer[len] = (self.buffer[len] << length) + value;
                self.last_size += length;
            } else {
                let (prefix, suffix) = split_number_by_prefix(value, length as u8, free_bits as u8);
                self.buffer[len] = (self.buffer[len] << free_bits) + prefix;
                self.buffer.push(suffix);
                self.last_size = length - free_bits;
            }
        }
        self.size += length;
    }

    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut rtn = Vec::new();
        // Return if nothing to work on
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let mut i = 0;
        while i < self.buffer.len() - 1 {
            rtn.extend(self.buffer[i].to_be_bytes());
            i += 1;
        }
        let size = ucdiv_t(self.last_size, 8);
        rtn.extend((self.buffer[i] << (64 - self.last_size)).to_be_bytes()[0..size].to_vec());
        rtn
    }

    #[allow(clippy::cast_possible_truncation)]
    #[must_use]
    pub fn slice_to_int(&self, start_index: usize, end_index: usize) -> u64 {
        if start_index >> 6 == end_index >> 6 {
            let mut res: u64 = self.buffer[start_index >> 6];
            if start_index >> 6 == self.buffer.len() - 1 {
                res >>= self.last_size - (end_index & 63);
            } else {
                res >>= 64 - (end_index & 63);
            }
            res &= (1u64 << ((end_index & 63) - (start_index & 63))) - 1;
            res
        } else {
            debug_assert_eq!((start_index >> 6) + 1, (end_index >> 6));
            let mut split =
                split_number_by_prefix(self.buffer[start_index >> 6], 64, (start_index & 63) as u8);
            let mut result = split.1;
            if end_index % 64 > 0 {
                let bucket_size = if end_index >> 6 == self.buffer.len() - 1 {
                    self.last_size
                } else {
                    64
                };
                split = split_number_by_prefix(
                    self.buffer[end_index >> 6],
                    bucket_size as u8,
                    (end_index & 63) as u8,
                );
                result = (result << (end_index & 63)) + split.0;
            }
            result
        }
    }
    #[must_use]
    pub fn first_u64(&self) -> u64 {
        if self.buffer.is_empty() {
            0
        } else {
            self._read_u64(self.last_size, 0)
        }
    }

    pub fn read_u64(&mut self, bit_count: usize) -> std::io::Result<u64> {
        if self.position + bit_count > self.size {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Seek is past end of buffer: {} > {}",
                    self.position, self.size
                ),
            ))
        } else {
            let res = self._read_u64(bit_count, self.position);
            self.position += bit_count;
            Ok(res)
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn _read_u64(&self, bit_count: usize, position: usize) -> u64 {
        debug_assert!(bit_count <= 64);
        let mut start_index = position;
        let mut end_index = position + bit_count;
        let start_bucket = start_index / 64;
        let end_bucket = end_index / 64;
        if start_bucket == end_bucket {
            // Positions inside the bucket.
            start_index %= 64;
            end_index %= 64;
            let bucket_size = if start_bucket == (self.buffer.len() - 1) {
                self.last_size
            } else {
                64
            }; //u8?
            let mut val = self.buffer[start_bucket];
            // Cut the prefix [0, start_index)
            if start_index != 0 {
                val &= (1u64 << (bucket_size - start_index)) - 1;
            }
            // Cut the suffix after end_index
            val >>= bucket_size - end_index;
            val
        } else {
            // Get the prefix from the last bucket.
            let mut split =
                split_number_by_prefix(self.buffer[start_bucket], 64, (start_index % 64) as u8);
            let mut result = split.1;
            if end_index % 64 > 0 {
                let bucket_size = if end_bucket == (self.buffer.len() - 1) {
                    self.last_size
                } else {
                    64
                };
                // Get the suffix from the last bucket.
                let end_size = end_index % 64;
                split = split_number_by_prefix(
                    self.buffer[end_bucket],
                    bucket_size as u8,
                    end_size as u8,
                );
                result = (result << end_size) | split.0;
            }
            result
        }
    }

    #[must_use]
    pub fn get_size(&self) -> usize {
        if self.buffer.is_empty() {
            0
        } else {
            (self.buffer.len() - 1) * 64 + self.last_size
        }
    }

    #[must_use]
    pub fn slice(&self, start_index: usize) -> Self {
        self.range(start_index, self.get_size())
    }

    #[allow(clippy::cast_possible_truncation)]
    #[must_use]
    pub fn range(&self, start_index: usize, end_index: usize) -> Self {
        let mut start_index = start_index;
        let mut end_index = end_index;
        if end_index > self.get_size() {
            end_index = self.get_size();
        }
        if end_index == start_index {
            return BitReader::default();
        }
        debug_assert!(end_index > start_index);
        let start_bucket = start_index / 64;
        let end_bucket = end_index / 64;
        if start_bucket == end_bucket {
            // Positions inside the bucket.
            start_index %= 64;
            end_index %= 64;
            let bucket_size = if start_bucket == (self.buffer.len() - 1) {
                self.last_size
            } else {
                64
            };
            let mut val = self.buffer[start_bucket];
            if start_index != 0 {
                val &= (1u64 << (bucket_size - start_index)) - 1;
            }
            val >>= bucket_size - end_index;
            BitReader::new(val, end_index - start_index)
        } else {
            let mut result = BitReader::default();
            let mut split =
                split_number_by_prefix(self.buffer[start_bucket], 64, (start_index % 64) as u8);
            result.append_value(split.1, 64 - start_index % 64);
            let mut i = start_bucket + 1;
            while i < end_bucket {
                result.append_value(self.buffer[i], 64);
                i += 1;
            }
            if end_index % 64 > 0 {
                let bucket_size = if end_bucket == (self.buffer.len() - 1) {
                    self.last_size
                } else {
                    64
                }; //u8?
                   // Get the suffix from the last bucket.
                split = split_number_by_prefix(
                    self.buffer[end_bucket],
                    bucket_size as u8,
                    (end_index % 64) as u8,
                );
                result.append_value(split.0, end_index % 64);
            }
            result
        }
    }
}
impl Seek for BitReader {
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(p) => {
                if p as usize > self.size {
                    Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Seek is past end of buffer: {} > {}", p, self.size),
                    ))
                } else {
                    self.position = p as usize;
                    Ok(self.position as u64)
                }
            }
            SeekFrom::End(p) => {
                let p = self.size as i64 + p;
                if p as usize > self.size || p < 0 {
                    Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Seek is out of bounds. Len {}, Seek {p}", self.size),
                    ))
                } else {
                    self.position = p as usize;
                    Ok(self.position as u64)
                }
            }
            SeekFrom::Current(p) => {
                let p = self.position as i64 + p;
                if p as usize > self.size || p < 0 {
                    Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("Seek is out of bounds. Len {}, Seek {p}", self.size),
                    ))
                } else {
                    self.position = p as usize;
                    Ok(self.position as u64)
                }
            }
        }
    }
}
#[inline]
fn split_number_by_prefix(number: u64, num_bits: u8, prefix_size: u8) -> (u64, u64) {
    if prefix_size == 0 {
        (0, number)
    } else {
        let shift_amt = num_bits - prefix_size;
        (number >> shift_amt, number & ((1u64 << shift_amt) - 1))
    }
}
impl ops::Add<BitReader> for BitReader {
    type Output = BitReader;

    fn add(self, rhs: BitReader) -> BitReader {
        self + &rhs
    }
}
impl ops::Add<&BitReader> for BitReader {
    type Output = BitReader;

    fn add(self, rhs: &BitReader) -> BitReader {
        let mut rtn = self;
        if !rhs.buffer.is_empty() {
            let mut i = 0;
            while i < rhs.buffer.len() - 1 {
                rtn.append_value(rhs.buffer[i], 64);
                i += 1;
            }
            rtn.append_value(rhs.buffer[rhs.buffer.len() - 1], rhs.last_size);
        }
        rtn
    }
}
impl ops::AddAssign<&BitReader> for BitReader {
    fn add_assign(&mut self, rhs: &BitReader) {
        if !rhs.buffer.is_empty() {
            let mut i = 0;
            while i < rhs.buffer.len() - 1 {
                self.append_value(rhs.buffer[i], 64);
                i += 1;
            }
            self.append_value(rhs.buffer[rhs.buffer.len() - 1], rhs.last_size);
        }
    }
}
impl ops::AddAssign<&mut BitReader> for BitReader {
    fn add_assign(&mut self, rhs: &mut BitReader) {
        if !rhs.buffer.is_empty() {
            let mut i = 0;
            while i < rhs.buffer.len() - 1 {
                self.append_value(rhs.buffer[i], 64);
                i += 1;
            }
            self.append_value(rhs.buffer[rhs.buffer.len() - 1], rhs.last_size);
        }
    }
}
impl PartialEq for BitReader {
    fn eq(&self, other: &Self) -> bool {
        self.buffer == other.buffer && self.last_size == other.last_size
    }
}
impl Eq for BitReader {}

impl PartialOrd for BitReader {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for BitReader {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.get_size() != other.get_size() {
            return self.get_size().cmp(&other.get_size());
        }
        let mut i = 0;
        while i < self.buffer.len() {
            if self.buffer[i] < other.buffer[i] {
                return Ordering::Less;
            }
            if self.buffer[i] > other.buffer[i] {
                return Ordering::Greater;
            }
            i += 1;
        }
        Ordering::Equal
    }
}
