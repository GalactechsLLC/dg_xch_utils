use crate::constants::ucdiv;
use log::error;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter, Write};
use std::ops;

#[derive(Clone)]
pub struct BitVec {
    pub values: Vec<u64>,
    last_size: u32,
}
impl Display for BitVec {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut i = 0;
        while i < self.values.len() {
            let mut val: u64 = self.values[i];
            let size = if i == self.values.len() - 1 {
                self.last_size
            } else {
                64
            };
            let mut i2 = 0;
            while i2 < size {
                if val % 2 > 0 {
                    f.write_char('1')?;
                } else {
                    f.write_char('0')?;
                }
                val /= 2;
                i2 += 1;
            }
            i += 1;
        }
        Ok(())
    }
}
impl BitVec {
    pub fn new(value: u128, size: u32) -> Self {
        let mut bits: BitVec = BitVec {
            values: Vec::new(),
            last_size: 0,
        };
        if size > 64 {
            bits.init_bits((value >> 64) as u64, size - 64);
            bits.append_value(value as u64, 64);
        } else {
            bits.init_bits(value as u64, size);
        }
        bits
    }

    fn init_bits(&mut self, value: u64, size: u32) {
        self.last_size = 0;
        if size > 64 {
            // Get number of extra 0s added at the beginning.
            let mut zeros = size - value.checked_ilog2().map(|u| u + 1).unwrap_or_default();
            // Add a full group of 0s (length 64)
            while zeros > 64 {
                self.append_value(0, 64);
                zeros -= 64;
            }
            // Add the incomplete group of 0s and then the value.
            self.append_value(0, zeros);
            self.append_value(
                value,
                value.checked_ilog2().map(|u| u + 1).unwrap_or_default(),
            );
        } else {
            /* 'value' must be under 'size' bits. */
            assert!(size == 64 || value == (value & ((1u64 << size) - 1)));
            self.values.push(value);
            self.last_size = size;
        }
    }

    pub fn from_other(other: &BitVec) -> Self {
        BitVec {
            values: other.values.clone(),
            last_size: other.last_size,
        }
    }

    pub fn from_other_sized(other: &BitVec, size: u32) -> Self {
        let mut bits: BitVec = BitVec {
            values: Vec::new(),
            last_size: 0,
        };
        let total_size = other.get_size();
        assert!(size >= total_size);
        // Add the extra 0 bits at the beginning.
        let mut extra_space = size - total_size;
        while extra_space >= 64 {
            bits.append_value(0, 64);
            extra_space -= 64;
        }
        if extra_space > 0 {
            bits.append_value(0, extra_space);
        }
        // Copy the Bits object element by element, and append it to the current Bits object.
        if !other.values.is_empty() {
            let mut index = 0;
            while index < other.values.len() {
                bits.append_value(other.values[index], 64);
                index += 1;
            }
            bits.append_value(other.values[other.values.len() - 1], other.last_size);
        }
        bits
    }

    pub fn from_be_bytes(
        big_endian_bytes: impl AsRef<[u8]>,
        num_bytes: u32,
        size_bits: u32,
    ) -> Self {
        let big_endian_bytes = big_endian_bytes.as_ref();
        let mut bits: BitVec = BitVec {
            values: Vec::new(),
            last_size: 0,
        };
        if big_endian_bytes.is_empty() {
            return bits;
        }
        let mut extra_space = size_bits - num_bytes * 8;
        while extra_space >= 64 {
            bits.append_value(0, 64);
            extra_space -= 64;
        }
        if extra_space > 0 {
            bits.append_value(0, extra_space);
        }
        let mut i = 0;
        while i < num_bytes {
            let mut val = 0u64;
            let mut bucket_size = 0;
            // Compress bytes together into u64, either until we have 64 bits, or until we run
            // out of bytes in big_endian_bytes.
            let mut j = i;
            while j < i + u64::BITS / u8::BITS && j < num_bytes {
                val = (val << 8) + big_endian_bytes[j as usize] as u64;
                bucket_size += 8;
                j += 1;
            }
            bits.append_value(val, bucket_size);
            i += u64::BITS / u8::BITS;
        }
        bits
    }

    pub fn slice(&self, start_index: u32) -> Self {
        self.range(start_index, self.get_size())
    }

    pub fn range(&self, start_index: u32, end_index: u32) -> Self {
        let mut start_index = start_index;
        let mut end_index = end_index;
        if end_index > self.get_size() {
            end_index = self.get_size();
        }
        if end_index == start_index {
            return BitVec {
                values: Vec::new(),
                last_size: 0,
            };
        }
        assert!(end_index > start_index);
        let start_bucket = start_index / 64;
        let end_bucket = end_index / 64;
        if start_bucket == end_bucket {
            // Positions inside the bucket.
            start_index %= 64;
            end_index %= 64;
            let bucket_size = if start_bucket as usize == (self.values.len() - 1) {
                self.last_size
            } else {
                64
            }; //u8?
            let mut val = self.values[start_bucket as usize];
            // Cut the prefix [0, start_index)
            if start_index != 0 {
                val &= (1u64 << (bucket_size - start_index)) - 1;
            }
            // Cut the suffix after end_index
            val >>= bucket_size - end_index;
            BitVec::new(val.into(), end_index - start_index)
        } else {
            let mut result = BitVec {
                values: Vec::new(),
                last_size: 0,
            };
            // Get the prefix from the last bucket.
            let mut split = split_number_by_prefix(
                self.values[start_bucket as usize],
                64,
                (start_index % 64) as u8,
            );
            result.append_value(split.1, 64 - start_index % 64);
            // Append all the in between buckets
            let mut i = start_bucket + 1;
            while i < end_bucket {
                result.append_value(self.values[i as usize], 64);
                i += 1;
            }
            if end_index % 64 > 0 {
                let bucket_size = if end_bucket == (self.values.len() - 1) as u32 {
                    self.last_size
                } else {
                    64
                }; //u8?
                   // Get the suffix from the last bucket.
                split = split_number_by_prefix(
                    self.values[end_bucket as usize],
                    bucket_size as u8,
                    (end_index % 64) as u8,
                );
                result.append_value(split.0, end_index % 64);
            }
            result
        }
    }

    pub fn slice_to_int(&self, start_index: u32, end_index: u32) -> u64 {
        if (start_index >> 6) == (end_index >> 6) {
            let mut res: u64 = self.values[(start_index >> 6) as usize];
            if (start_index >> 6) as usize == self.values.len() - 1 {
                res >>= self.last_size - (end_index & 63);
            } else {
                res >>= 64 - (end_index & 63);
            }
            res &= (1u64 << ((end_index & 63) - (start_index & 63))) - 1;
            res
        } else {
            assert_eq!((start_index >> 6) + 1, (end_index >> 6));
            let mut split = split_number_by_prefix(
                self.values[(start_index >> 6) as usize],
                64,
                (start_index & 63) as u8,
            );
            let mut result = split.1;
            if end_index % 64 > 0 {
                let bucket_size = if (end_index >> 6) as usize == self.values.len() - 1 {
                    self.last_size
                } else {
                    64
                };
                split = split_number_by_prefix(
                    self.values[(end_index >> 6) as usize],
                    bucket_size as u8,
                    (end_index & 63) as u8,
                );
                result = (result << (end_index & 63)) + split.0;
            }
            result
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut rtn = Vec::new();
        // Return if nothing to work on
        if !self.values.len() == 0 {
            return Vec::new();
        }
        let mut i = 0;
        while i < self.values.len() - 1 {
            rtn.extend(self.values[i].to_be_bytes());
            i += 1;
        }
        let size = ucdiv(self.last_size, 8);
        rtn.extend(
            (self.values[i] << (64 - self.last_size)).to_be_bytes()[0..size as usize].to_vec(),
        );
        rtn
    }

    pub fn get_value(&self) -> Option<u64> {
        if self.values.len() != 1 {
            error!("Number doesn't fit into a 64-bit type. {}", self.get_size());
            None
        } else {
            Some(self.values[0])
        }
    }
    pub fn get_value_unchecked(&self) -> u64 {
        self.values[0]
    }

    pub fn get_size(&self) -> u32 {
        if self.values.is_empty() {
            0
        } else {
            // Full buckets contain each 64 bits, last one contains only 'last_size' bits.
            (self.values.len() as u32 - 1) * 64 + self.last_size
        }
    }

    fn append_value(&mut self, value: u64, length: u32) {
        // The last bucket is full or no bucket yet, create a new one.
        if self.values.is_empty() || self.last_size == 64 {
            self.values.push(value);
            self.last_size = length;
        } else {
            let free_bits = 64 - self.last_size;
            let len: usize = self.values.len() - 1;
            if self.last_size == 0 && length == 64 {
                self.values[len] = value;
                self.last_size = length;
            } else if length <= free_bits {
                // If the value fits into the last bucket, append it all there.
                self.values[len] = (self.values[len] << length) + value;
                self.last_size += length;
            } else {
                // Otherwise, append the prefix into the last bucket, and create a new bucket for
                // the suffix.
                let (prefix, suffix) = split_number_by_prefix(value, length as u8, free_bits as u8);
                self.values[len] = (self.values[len] << free_bits) + prefix;
                self.values.push(suffix);
                self.last_size = length - free_bits;
            }
        }
    }
}
#[inline]
fn split_number_by_prefix(number: u64, num_bits: u8, prefix_size: u8) -> (u64, u64) {
    assert!(num_bits >= prefix_size);
    if prefix_size == 0 {
        (0, number)
    } else {
        let shift_amt = num_bits - prefix_size;
        (number >> shift_amt, number & ((1u64 << shift_amt) - 1))
    }
}
impl ops::Add<BitVec> for BitVec {
    type Output = BitVec;

    fn add(self, rhs: BitVec) -> BitVec {
        self + &rhs
    }
}
impl ops::Add<&BitVec> for BitVec {
    type Output = BitVec;

    fn add(self, _rhs: &BitVec) -> BitVec {
        let mut rtn = self;
        if !_rhs.values.is_empty() {
            let mut i = 0;
            while i < _rhs.values.len() - 1 {
                rtn.append_value(_rhs.values[i], 64);
                i += 1;
            }
            rtn.append_value(_rhs.values[_rhs.values.len() - 1], _rhs.last_size);
        }
        rtn
    }
}
impl ops::AddAssign<BitVec> for BitVec {
    fn add_assign(&mut self, rhs: BitVec) {
        *self += &rhs
    }
}
impl ops::AddAssign<&BitVec> for BitVec {
    fn add_assign(&mut self, _rhs: &BitVec) {
        if !_rhs.values.is_empty() {
            let mut i = 0;
            while i < _rhs.values.len() - 1 {
                self.append_value(_rhs.values[i], 64);
                i += 1;
            }
            self.append_value(_rhs.values[_rhs.values.len() - 1], _rhs.last_size);
        }
    }
}
impl PartialEq for BitVec {
    fn eq(&self, other: &Self) -> bool {
        self.values == other.values && self.last_size == other.last_size
    }
}
impl Eq for BitVec {}

impl PartialOrd for BitVec {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for BitVec {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.get_size() != other.get_size() {
            return self.get_size().cmp(&other.get_size());
        }
        let mut i = 0;
        while i < self.values.len() {
            if self.values[i] < other.values[i] {
                return Ordering::Less;
            }
            if self.values[i] > other.values[i] {
                return Ordering::Greater;
            }
            i += 1;
        }
        Ordering::Equal
    }
}
