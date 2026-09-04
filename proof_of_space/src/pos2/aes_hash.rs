use aes::cipher::generic_array::GenericArray;
use aes::hazmat::cipher_round;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::traits::SizedBytes;
#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::{uint8x16_t, vaeseq_u8, vaesmcq_u8, vdupq_n_u8, veorq_u8};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    __m128i, _mm_aesenc_si128, _mm_cvtsi128_si32, _mm_loadu_si128, _mm_set_epi32, _mm_storeu_si128,
};

/// Round counts for each use of the plot hash.
pub const AES_G_ROUNDS: u32 = 16;
pub const AES_PAIRING_ROUNDS: u32 = 16;
pub const AES_MATCHING_TARGET_ROUNDS: u32 = 16;
pub const AES_CHAINING_ROUNDS: u32 = 16;

/// True when the CPU carries AES instructions, checked once rather than per hash.
fn has_native_aes() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("aes")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// The plot's hash function: AES rounds keyed by the two halves of the plot id.
///
/// The 128 bit state is four little endian 32 bit lanes, matching `_mm_set_epi32(i3, i2, i1, i0)`
/// with `i0` in the low lane, and each round applies `aesenc` against both keys in turn.
#[derive(Debug, Clone)]
pub struct AesHash {
    key1: [u8; 16],
    key2: [u8; 16],
    k: u8,
    native: bool,
}

impl AesHash {
    #[must_use]
    pub fn new(plot_id: &Bytes32, k: u8) -> Self {
        let bytes = plot_id.bytes();
        let mut key1 = [0u8; 16];
        let mut key2 = [0u8; 16];
        key1.copy_from_slice(&bytes[0..16]);
        key2.copy_from_slice(&bytes[16..32]);
        Self {
            key1,
            key2,
            k,
            native: has_native_aes(),
        }
    }

    fn state(i0: u32, i1: u32, i2: u32, i3: u32) -> [u8; 16] {
        let mut state = [0u8; 16];
        state[0..4].copy_from_slice(&i0.to_le_bytes());
        state[4..8].copy_from_slice(&i1.to_le_bytes());
        state[8..12].copy_from_slice(&i2.to_le_bytes());
        state[12..16].copy_from_slice(&i3.to_le_bytes());
        state
    }

    fn lane(state: &[u8; 16], index: usize) -> u32 {
        let mut word = [0u8; 4];
        word.copy_from_slice(&state[index * 4..index * 4 + 4]);
        u32::from_le_bytes(word)
    }

    fn apply(&self, state: [u8; 16], rounds: u32) -> [u8; 16] {
        if self.native {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: `native` is only set when the aes feature was detected at runtime.
            return unsafe { self.apply_x86(state, rounds) };
            #[cfg(target_arch = "aarch64")]
            // SAFETY: `native` is only set when the aes feature was detected at runtime.
            return unsafe { self.apply_aarch64(state, rounds) };
        }
        self.apply_portable(state, rounds)
    }

    /// The portable path: correct everywhere, but a bitsliced software round is several times the
    /// cost of the instruction, so it is a fallback rather than the plotting path.
    fn apply_portable(&self, mut state: [u8; 16], rounds: u32) -> [u8; 16] {
        let mut block = GenericArray::clone_from_slice(&state);
        let key1 = GenericArray::clone_from_slice(&self.key1);
        let key2 = GenericArray::clone_from_slice(&self.key2);
        for _ in 0..rounds {
            cipher_round(&mut block, &key1);
            cipher_round(&mut block, &key2);
        }
        state.copy_from_slice(block.as_slice());
        state
    }

    /// Keys and state stay in registers for the whole round loop, which is what makes this
    /// latency bound on `aesenc` rather than on moving bytes.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "aes")]
    unsafe fn apply_x86(&self, state: [u8; 16], rounds: u32) -> [u8; 16] {
        unsafe {
            let key1 = _mm_loadu_si128(self.key1.as_ptr().cast::<__m128i>());
            let key2 = _mm_loadu_si128(self.key2.as_ptr().cast::<__m128i>());
            let mut block = _mm_loadu_si128(state.as_ptr().cast::<__m128i>());
            for _ in 0..rounds {
                block = _mm_aesenc_si128(block, key1);
                block = _mm_aesenc_si128(block, key2);
            }
            let mut out = [0u8; 16];
            _mm_storeu_si128(out.as_mut_ptr().cast::<__m128i>(), block);
            out
        }
    }

    /// `aesenc(state, key)` is `MixColumns(SubBytes(ShiftRows(state))) ^ key`; on aarch64 that is
    /// `vaeseq_u8` against a zero key, then `vaesmcq_u8`, then the key xor.
    #[cfg(target_arch = "aarch64")]
    #[target_feature(enable = "aes")]
    unsafe fn apply_aarch64(&self, state: [u8; 16], rounds: u32) -> [u8; 16] {
        unsafe {
            let zero = vdupq_n_u8(0);
            let key1 = std::ptr::read_unaligned(self.key1.as_ptr().cast::<uint8x16_t>());
            let key2 = std::ptr::read_unaligned(self.key2.as_ptr().cast::<uint8x16_t>());
            let mut block = std::ptr::read_unaligned(state.as_ptr().cast::<uint8x16_t>());
            for _ in 0..rounds {
                block = veorq_u8(vaesmcq_u8(vaeseq_u8(block, zero)), key1);
                block = veorq_u8(vaesmcq_u8(vaeseq_u8(block, zero)), key2);
            }
            let mut out = [0u8; 16];
            std::ptr::write_unaligned(out.as_mut_ptr().cast::<uint8x16_t>(), block);
            out
        }
    }

    /// Mask for the low `k` bits. A `k` of 32 or more keeps the whole word rather than shifting by
    /// the word width.
    fn k_mask(&self) -> u32 {
        if self.k >= 32 {
            u32::MAX
        } else {
            (1u32 << self.k) - 1
        }
    }

    /// `g` over many x values at once, writing `match_info` for each into `out`.
    ///
    /// Each call is independent, so hashing a block of them in one call keeps several `aesenc`
    /// chains in flight. A scalar `g_x` cannot: the instruction path sits behind a `target_feature`
    /// boundary the optimiser will not inline through.
    ///
    /// # Panics
    /// If `out` is shorter than `xs`.
    pub fn g_x_batch(&self, xs: &[u32], out: &mut [u32], rounds: u32) {
        assert!(
            out.len() >= xs.len(),
            "output buffer is shorter than the input"
        );
        #[cfg(target_arch = "x86_64")]
        if self.native {
            // SAFETY: `native` is only set when the aes feature was detected at runtime.
            unsafe { self.g_x_batch_x86(xs, out, rounds) };
            return;
        }
        for (x, slot) in xs.iter().zip(out.iter_mut()) {
            *slot = self.g_x(*x, rounds);
        }
    }

    /// Eight lanes is enough to cover `aesenc` latency on the processors this runs on without
    /// spilling the round state out of registers.
    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "aes")]
    unsafe fn g_x_batch_x86(&self, xs: &[u32], out: &mut [u32], rounds: u32) {
        const LANES: usize = 8;
        unsafe {
            let key1 = _mm_loadu_si128(self.key1.as_ptr().cast::<__m128i>());
            let key2 = _mm_loadu_si128(self.key2.as_ptr().cast::<__m128i>());
            let mask = self.k_mask();
            let mut i = 0;
            while i + LANES <= xs.len() {
                let mut block: [__m128i; LANES] =
                    std::array::from_fn(|j| _mm_set_epi32(0, 0, 0, xs[i + j] as i32));
                for _ in 0..rounds {
                    for b in &mut block {
                        *b = _mm_aesenc_si128(*b, key1);
                    }
                    for b in &mut block {
                        *b = _mm_aesenc_si128(*b, key2);
                    }
                }
                for (j, b) in block.iter().enumerate() {
                    out[i + j] = (_mm_cvtsi128_si32(*b) as u32) & mask;
                }
                i += LANES;
            }
            while i < xs.len() {
                out[i] = self.g_x(xs[i], rounds);
                i += 1;
            }
        }
    }

    /// `g(x)`: the function that turns an x value into its `match_info`.
    #[must_use]
    pub fn g_x(&self, x: u32, rounds: u32) -> u32 {
        let state = self.apply(Self::state(x, 0, 0, 0), rounds);
        Self::lane(&state, 0) & self.k_mask()
    }

    /// The target a left entry projects onto, which a right entry's `match_target` must equal.
    ///
    /// `extra_rounds_bits` multiplies the work by `1 << bits`; table 1 uses it to spend the plot's
    /// strength, which is what makes plotting expensive without costing verification.
    #[must_use]
    pub fn matching_target(
        &self,
        table_id: u32,
        match_key: u32,
        meta: u64,
        extra_rounds_bits: u32,
    ) -> u32 {
        let state = Self::state(table_id, match_key, meta as u32, (meta >> 32) as u32);
        let rounds = AES_MATCHING_TARGET_ROUNDS << extra_rounds_bits;
        Self::lane(&self.apply(state, rounds), 0)
    }

    /// The four lane result a pairing produces: match info, two words of metadata, and test bits.
    #[must_use]
    pub fn pairing(&self, meta_l: u64, meta_r: u64, extra_rounds_bits: u32) -> [u32; 4] {
        let state = Self::state(
            meta_l as u32,
            (meta_l >> 32) as u32,
            meta_r as u32,
            (meta_r >> 32) as u32,
        );
        let rounds = AES_PAIRING_ROUNDS << extra_rounds_bits;
        let state = self.apply(state, rounds);
        [
            Self::lane(&state, 0),
            Self::lane(&state, 1),
            Self::lane(&state, 2),
            Self::lane(&state, 3),
        ]
    }

    /// The hash that links proof fragments into a quality chain.
    #[must_use]
    pub fn chain(&self, input: u64) -> u64 {
        let state = Self::state(input as u32, (input >> 32) as u32, 0, 0);
        let state = self.apply(state, AES_CHAINING_ROUNDS);
        u64::from(Self::lane(&state, 0)) | (u64::from(Self::lane(&state, 1)) << 32)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/pos2/aes_hash/tests.rs"]
mod tests;
