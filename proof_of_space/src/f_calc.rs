use crate::chacha8::{chacha8_get_keystream, chacha8_keysetup, ChachaContext};
use crate::constants::{
    cdiv, ucdiv, ucdiv64, PlotEntry, K_BC, K_EXTRA_BITS, K_EXTRA_BITS_POW, K_F1_BLOCK_SIZE_BITS,
    K_VECTOR_LENS, L_TARGETS,
};
use crate::utils::bit_reader::BitReader;
use crate::utils::slice_u64from_bytes;
use std::cmp::min;
use std::io::Error;

pub struct F1Calculator {
    k: u8,
    enc_ctx_: ChachaContext,
}
impl F1Calculator {
    #[must_use]
    pub fn new(k: u8, orig_key: &[u8; 32]) -> F1Calculator {
        let mut f1_calc = F1Calculator {
            k,
            enc_ctx_: ChachaContext { input: [0; 16] },
        };
        f1_calc.init(orig_key);
        f1_calc
    }
    fn init(&mut self, orig_key: &[u8; 32]) {
        // First byte is 1, the index of this table
        let mut enc_key: [u8; 32] = [0; 32];
        enc_key[0] = 1;
        enc_key[1..].clone_from_slice(&orig_key[0..31]);
        // Setup ChaCha8 context with zero-filled IV
        chacha8_keysetup(&mut self.enc_ctx_, &enc_key, None);
    }
    #[allow(clippy::cast_possible_truncation)]
    pub fn calculate_f(&self, l: &BitReader) -> Result<BitReader, Error> {
        let num_output_bits = u16::from(self.k);
        let block_size_bits = K_F1_BLOCK_SIZE_BITS;

        // Calculates the counter that will be used to get ChaCha8 keystream.
        // Since k < block_size_bits, we can fit several k bit blocks into one
        // ChaCha8 block.
        let counter_bit: u128 = l.first_u64() as u128 * num_output_bits as u128;
        let mut counter: u64 = (counter_bit / block_size_bits as u128) as u64;

        // How many bits are before L, in the current block
        let bits_before_l: u32 = (counter_bit % block_size_bits as u128) as u32;

        // How many bits of L are in the current block (the rest are in the next block)
        let bits_of_l = min(
            (u32::from(block_size_bits) - bits_before_l) as u16,
            num_output_bits,
        );

        // True if L is divided into two blocks, and therefore 2 ChaCha8
        // keystream blocks will be generated.
        let spans_two_blocks: bool = bits_of_l < num_output_bits;

        let mut ciphertext_bytes: Vec<u8> = vec![0; 64];
        let mut output_bits: BitReader;

        // This counter is used to initialize words 12 and 13 of ChaCha8
        // initial state (4x4 matrix of 32-bit words). This is similar to
        // encrypting plaintext at a given offset, but we have no
        // plaintext, so no XORing at the end.
        chacha8_get_keystream(&self.enc_ctx_, counter, 1, &mut ciphertext_bytes);
        let ciphertext0 = BitReader::from_bytes_be(&ciphertext_bytes, block_size_bits as usize);

        if spans_two_blocks {
            // Performs another encryption if necessary
            counter += 1;
            ciphertext_bytes.clear();
            chacha8_get_keystream(&self.enc_ctx_, counter, 1, &mut ciphertext_bytes);
            let ciphertext1: BitReader =
                BitReader::from_bytes_be(&ciphertext_bytes, block_size_bits as usize);
            output_bits = ciphertext0.slice(bits_before_l as usize)
                + ciphertext1.range(0, (num_output_bits - bits_of_l).into());
        } else {
            output_bits = ciphertext0.range(
                bits_before_l as usize,
                (bits_before_l + u32::from(num_output_bits)) as usize,
            );
        }

        // Adds the first few bits of L to the end of the output, production k + kExtraBits of output
        let mut extra_data = l.range(0, K_EXTRA_BITS.into());
        if extra_data.get_size() < K_EXTRA_BITS as usize {
            extra_data += &BitReader::new(0, K_EXTRA_BITS as usize - extra_data.get_size());
        }
        output_bits += &extra_data;
        Ok(output_bits)
    }
    pub fn calculate_bucket(&self, l: &BitReader) -> Result<(BitReader, BitReader), Error> {
        Ok((self.calculate_f(l)?, l.clone()))
    }

    // F1(x) values for x in range [first_x, first_x + n) are placed in res[].
    // n must not be more than 1 << kBatchSizes.
    #[allow(clippy::cast_possible_truncation)]
    pub fn calculate_buckets(&self, first_x: u64, n: u64, res: &mut [u64]) {
        let start = first_x * u64::from(self.k) / u64::from(K_F1_BLOCK_SIZE_BITS);
        // 'end' is one past the last keystream block number to be generated
        let end: u64 = ucdiv64(
            (first_x + n) * u64::from(self.k),
            u64::from(K_F1_BLOCK_SIZE_BITS),
        );
        let num_blocks: u64 = end - start;
        let mut start_bit: u32 =
            (first_x * u64::from(self.k) % u64::from(K_F1_BLOCK_SIZE_BITS)) as u32;
        let x_shift: u8 = self.k - K_EXTRA_BITS;
        //assert(n <= (1U << kBatchSizes));
        let mut ciphertext_bytes: Vec<u8> = Vec::new();
        chacha8_get_keystream(
            &self.enc_ctx_,
            start,
            num_blocks as u32,
            &mut ciphertext_bytes,
        );
        for x in first_x..(first_x + n) {
            let y = slice_u64from_bytes(&ciphertext_bytes, start_bit, u32::from(self.k));
            res[(x - first_x) as usize] = (y << K_EXTRA_BITS) | (x >> x_shift);
            start_bit += u32::from(self.k);
        }
    }
}

#[derive(Clone, Debug)]
struct RmapItem {
    pub count: u16,
    pub pos: u16,
}
impl Default for RmapItem {
    fn default() -> RmapItem {
        RmapItem { count: 4, pos: 12 }
    }
}

pub struct FXCalculator {
    k: u8,
    table_index: u8,
    rmap: Vec<RmapItem>,
    rmap_clean: Vec<u16>,
}
impl FXCalculator {
    #[must_use]
    pub fn new(k: u8, table_index: u8) -> FXCalculator {
        FXCalculator {
            k,
            table_index,
            rmap: vec![RmapItem { count: 0, pos: 0 }; K_BC],
            rmap_clean: vec![],
        }
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    #[allow(clippy::cast_possible_wrap)]
    #[must_use]
    pub fn calculate_bucket(
        &self,
        y1: &BitReader,
        l: &BitReader,
        r: &BitReader,
    ) -> (BitReader, BitReader) {
        let input: BitReader;
        let mut c: BitReader;
        if self.table_index < 4 {
            c = l.clone() + r;
            input = y1.clone() + &c;
        } else {
            c = BitReader::new(0, 0);
            input = y1.clone() + l + r;
        }

        let mut hasher = blake3::Hasher::new();
        let input_bytes = input.to_bytes();
        let byte_len = ucdiv(input.get_size() as u32, 8);
        hasher.update(&input_bytes[0..byte_len as usize]);
        let hash = hasher.finalize();
        let hash_bytes = hash.as_bytes();
        let mut u64_buffer: [u8; 8] = [0; 8];
        u64_buffer.copy_from_slice(&hash_bytes[0..8]);
        let f = u64::from_be_bytes(u64_buffer) >> (64 - (self.k + K_EXTRA_BITS));
        if self.table_index < 4 {
            c = l.clone() + r;
        } else if self.table_index < 7 {
            let len = K_VECTOR_LENS[(self.table_index + 1) as usize];
            let start_byte = ((self.k + K_EXTRA_BITS) / 8) as usize;
            let end_bit = (self.k + K_EXTRA_BITS + self.k * len) as usize;
            let end_byte = cdiv(end_bit as i32, 8i32) as usize;
            c = BitReader::from_bytes_be(
                &hash_bytes[start_byte..end_byte],
                (end_byte - start_byte) * 8,
            );
            c = c.range(
                ((self.k + K_EXTRA_BITS) % 8) as usize,
                end_bit - start_byte * 8,
            );
        }
        (BitReader::new(f, (self.k + K_EXTRA_BITS) as usize), c)
    }
    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_sign_loss)]
    pub fn find_matches(
        &mut self,
        bucket_l: &[PlotEntry],
        bucket_r: &[PlotEntry],
        mut idx_l: Option<&mut [u16]>,
        mut idx_r: Option<&mut [u16]>,
    ) -> i32 {
        let mut idx_count: i32 = 0;
        let parity: u16 = ((bucket_l[0].y / K_BC as u64) % 2) as u16;

        for yl in &self.rmap_clean {
            self.rmap[*yl as usize].count = 0;
        }
        self.rmap_clean.clear();

        let remove: u64 = (bucket_r[0].y / K_BC as u64) * K_BC as u64;
        let mut pos_r = 0;
        while pos_r < bucket_r.len() {
            let r_y: u64 = bucket_r[pos_r].y - remove;
            if self.rmap[r_y as usize].count == 0 {
                self.rmap[r_y as usize].pos = pos_r as u16;
            }
            self.rmap[r_y as usize].count += 1;
            self.rmap_clean.push(r_y as u16);
            pos_r += 1;
        }

        let remove_y: u64 = remove - K_BC as u64;
        let mut pos_l = 0;
        while pos_l < bucket_l.len() {
            let r: u64 = bucket_l[pos_l].y - remove_y;
            let mut i: usize = 0;
            while i < K_EXTRA_BITS_POW as usize {
                let r_target: u16 = L_TARGETS[parity as usize][r as usize][i];
                let mut j: usize = 0;
                while j < self.rmap[r_target as usize].count as usize {
                    if let Some(idx_l) = &mut idx_l {
                        idx_l[idx_count as usize] = pos_l as u16;
                        if let Some(idx_r) = &mut idx_r {
                            idx_r[idx_count as usize] = self.rmap[r_target as usize].pos + j as u16;
                        }
                    }
                    idx_count += 1;
                    j += 1;
                }
                i += 1;
            }
            pos_l += 1;
        }
        idx_count
    }
}
