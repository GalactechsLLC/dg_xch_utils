use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
use std::io::{Error, ErrorKind};

/// Feistel rounds, unless told otherwise.
pub const FEISTEL_ROUNDS: u32 = 4;

/// Low `bits` set, saturating rather than shifting by the word width.
fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// The block cipher that turns bit dropped x values into a proof fragment.
///
/// The block is `2k` bits split into two `k` bit halves, and each round mixes them with a
/// ChaCha20 style quarter round keyed by a `3k` bit slice of the plot id.
#[derive(Debug, Clone)]
pub struct FeistelCipher {
    plot_id: [u8; 32],
    k: u32,
    rounds: u32,
}

impl FeistelCipher {
    pub fn new(plot_id: Bytes32, k: u32) -> Result<Self, Error> {
        Self::with_rounds(plot_id, k, FEISTEL_ROUNDS)
    }

    pub fn with_rounds(plot_id: Bytes32, k: u32, rounds: u32) -> Result<Self, Error> {
        if k > 32 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("k {k} cannot exceed 32"),
            ));
        }
        if 3 * k > 256 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("3k for k {k} cannot exceed 256 bits"),
            ));
        }
        Ok(Self {
            plot_id: plot_id.bytes(),
            k,
            rounds,
        })
    }

    #[must_use]
    pub fn k(&self) -> u32 {
        self.k
    }

    fn rotate_left(value: u64, shift: u32, bit_length: u32) -> u64 {
        let shift = shift.min(bit_length);
        let m = mask(bit_length);
        ((value << shift) & m) | (value >> (bit_length - shift))
    }

    /// A big endian slice of the plot id. The segment is built in a 64 bit register, so a slice
    /// wider than 64 bits keeps only its low word; that truncation is part of the format.
    fn slice_key(&self, start_bit: usize, num_bits: usize) -> u64 {
        let start_byte = start_bit / 8;
        let bit_offset = start_bit % 8;
        let needed_bytes = (bit_offset + num_bits).div_ceil(8);
        if start_byte + needed_bytes > 32 {
            return 0;
        }
        let mut segment: u64 = 0;
        for i in 0..needed_bytes {
            segment = (segment << 8) | u64::from(self.plot_id[start_byte + i]);
        }
        let total_bits = needed_bytes * 8;
        let shift_amount = total_bits - bit_offset - num_bits;
        (segment >> shift_amount) & mask(u32::try_from(num_bits).unwrap_or(64))
    }

    fn round_key(&self, round: usize) -> u64 {
        let bits_for_round = 3 * self.k as usize;
        let start_bit = if self.rounds > 1 {
            round * (256 - bits_for_round) / (self.rounds as usize - 1)
        } else {
            0
        };
        self.slice_key(start_bit, bits_for_round)
    }

    fn round(&self, left: u64, right: u64, round_key: u64) -> (u64, u64) {
        let m = mask(self.k);
        // `wrapping_shr` keeps the shift-modulo-width behaviour the format depends on when `2k`
        // reaches the register width at k32.
        let mut a = right;
        let mut b = round_key & m;
        let mut c = round_key.wrapping_shr(self.k) & m;
        let mut d = round_key.wrapping_shr(2 * self.k) & m;

        a = a.wrapping_add(b) & m;
        d = Self::rotate_left(d ^ a, 16, self.k);
        c = c.wrapping_add(d) & m;
        b = Self::rotate_left(b ^ c, 12, self.k);

        a = a.wrapping_add(b) & m;
        d = Self::rotate_left(d ^ a, 8, self.k);
        c = c.wrapping_add(d) & m;
        b = Self::rotate_left(b ^ c, 7, self.k);

        (right, (left ^ b) & m)
    }

    #[must_use]
    pub fn encrypt(&self, input: u64) -> u64 {
        let m = mask(self.k);
        let mut left = input.wrapping_shr(self.k) & m;
        let mut right = input & m;
        for round in 0..self.rounds as usize {
            let key = self.round_key(round);
            let (l, r) = self.round(left, right, key);
            left = l;
            right = r;
        }
        (left << self.k) | right
    }

    #[must_use]
    pub fn decrypt(&self, cipher: u64) -> u64 {
        let m = mask(self.k);
        let mut left = cipher.wrapping_shr(self.k) & m;
        let mut right = cipher & m;
        for round in (0..self.rounds as usize).rev() {
            let key = self.round_key(round);
            // The inverse of a round is the same round with the halves swapped.
            let (l, r) = self.round(right, left, key);
            right = l;
            left = r;
        }
        (left << self.k) | right
    }
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/feistel/tests.rs"]
mod tests;
