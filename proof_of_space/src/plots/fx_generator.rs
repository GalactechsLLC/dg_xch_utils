use crate::chacha8::{
    chacha8_get_keystream, chacha8_get_keystream_unsafe, chacha8_keysetup, ChachaContext,
};
use crate::constants::{
    ucdiv_t, K_BC, K_EXTRA_BITS, K_F1_BLOCK_SIZE, K_F1_BLOCK_SIZE_BITS, L_TARGETS,
};
use crate::plots::{
    get_meta_in, get_meta_out, K32Meta1, K32Meta2, K32Meta3, K32Meta4, Pair, PROOF_X_COUNT,
};
use crate::utils::bit_reader::BitReader;
use crate::utils::radix_sort::RadixSorter;
use crate::utils::span::Span;
use crate::utils::{bytes_to_u64, calc_thread_vars, ThreadVars};
use blake3::Hasher;
use dg_xch_core::plots::PlotTable;
use log::debug;
use rayon::prelude::*;
use std::cmp::min;
use std::io::{Error, ErrorKind};
use std::mem::{size_of, swap};

#[allow(clippy::cast_possible_truncation)]
const F1ENTRIES_PER_BLOCK: u16 = K_F1_BLOCK_SIZE / size_of::<u32>() as u16;

#[derive(Debug)]
pub struct F1Generator {
    k: u8,
    thread_count: u8,
    context: ChachaContext,
}
impl F1Generator {
    #[must_use]
    pub fn new(k: u8, thread_count: u8, orig_key: &[u8; 32]) -> Self {
        // First byte is 1, the index of this table
        let mut enc_key: [u8; 32] = [0; 32];
        enc_key[0] = 1;
        enc_key[1..].clone_from_slice(&orig_key[0..31]);
        // Setup ChaCha8 context
        let mut context = ChachaContext { input: [0; 16] };
        chacha8_keysetup(&mut context, &enc_key, None);
        F1Generator {
            k,
            thread_count,
            context,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    #[allow(clippy::cast_possible_wrap)]
    #[allow(clippy::cast_sign_loss)]
    pub fn generate_f1(
        &self,
        bucket_entry_count: usize,
        x_sources: [u32; 2],
        x_tmp_buffer: &mut Vec<u32>,
        y_tmp_buffer: &mut Vec<u64>,
        x_out: &mut [u32],
        y_out: &mut Vec<u64>,
    ) -> Result<(), Error> {
        let x_shift = self.k - K_EXTRA_BITS;
        let f1blocks_per_bucket = bucket_entry_count * size_of::<u32>() / K_F1_BLOCK_SIZE as usize;
        let thread_count = min(self.thread_count as usize, f1blocks_per_bucket);
        let blocks: Span<u32> = Span::new(y_out.as_mut_ptr(), y_out.len()).cast();
        let x_entries: [Span<u32>; 2] = [
            Span::new(x_tmp_buffer.as_mut_ptr(), bucket_entry_count),
            Span::new(
                x_tmp_buffer[bucket_entry_count..].as_mut_ptr(),
                bucket_entry_count,
            ),
        ];
        let y_entries: [Span<u64>; 2] = [
            Span::new(y_tmp_buffer.as_mut_ptr(), bucket_entry_count),
            Span::new(
                y_tmp_buffer[bucket_entry_count..].as_mut_ptr(),
                bucket_entry_count,
            ),
        ];
        debug!("\t\tFXData: ({thread_count})");
        (0..thread_count)
            .map(|i| {
                let thread_vars = calc_thread_vars(i, thread_count, f1blocks_per_bucket);
                F1Job {
                    thread_vars,
                    blocks,
                    x_sources,
                    x_entries,
                    y_entries,
                }
            })
            .collect::<Vec<F1Job>>()
            .into_par_iter()
            .for_each(|job| unsafe {
                let entries_per_thread = job.thread_vars.count * F1ENTRIES_PER_BLOCK as usize;
                let entries_offset = job.thread_vars.offset * F1ENTRIES_PER_BLOCK as usize;
                let ciphertext_bytes: Span<u32> =
                    job.blocks.range(entries_offset, entries_per_thread);
                let mut x_slice;
                let mut y_slice;
                for ((x_source, x_entries), y_entries) in
                    job.x_sources.iter().zip(job.x_entries).zip(job.y_entries)
                {
                    let x_start = (x_source * bucket_entry_count as u32) + entries_offset as u32;
                    let block_index = u64::from(x_start) / u64::from(F1ENTRIES_PER_BLOCK);
                    x_slice = x_entries.slice(entries_offset);
                    y_slice = y_entries.slice(entries_offset);
                    chacha8_get_keystream_unsafe(
                        &self.context,
                        block_index,
                        job.thread_vars.count as u32,
                        ciphertext_bytes,
                    );
                    for j in 0..entries_per_thread as isize {
                        // Get the starting and end locations of y in bits relative to our block
                        let x = x_start + j as u32;
                        let mut y = u64::from(ciphertext_bytes[j].to_be());
                        y = (y << K_EXTRA_BITS) | u64::from(x >> x_shift);
                        x_slice[j] = x;
                        y_slice[j] = y;
                    }
                }
            });
        let merged_entry_count = bucket_entry_count * 2;
        debug!("\t\tFX Sort");
        RadixSorter::new(thread_count, merged_entry_count).sort_keyed(
            5,
            y_tmp_buffer,
            y_out,
            x_tmp_buffer,
            x_out,
        );
        Ok(())
    }
}

struct F1Job {
    thread_vars: ThreadVars<usize>,
    blocks: Span<u32>,
    x_sources: [u32; 2],
    x_entries: [Span<u32>; 2],
    y_entries: [Span<u64>; 2],
}
unsafe impl Send for F1Job {}
unsafe impl Sync for F1Job {}

#[allow(clippy::cast_possible_truncation)]
pub fn get_proof_f1_and_meta(
    k: u32,
    plot_id: &[u8; 32],
    proof: &[u64],
    fx: &mut [u64],
    meta: &mut Vec<BitReader>,
) -> Result<(), Error> {
    // Convert these x's to f1 values
    let x_shift = k - u32::from(K_EXTRA_BITS);
    // Prepare ChaCha key
    let mut enc_ctx: ChachaContext = ChachaContext { input: [0; 16] };
    let mut enc_key: [u8; 32] = [0; 32];
    enc_key[0] = 1;
    enc_key[1..].clone_from_slice(&plot_id[0..31]);
    chacha8_keysetup(&mut enc_ctx, &enc_key, None);
    // Enough to hold 2 cha-cha blocks since a value my span over 2 blocks
    let mut blocks = vec![];
    for (x, fx) in proof.iter().zip(fx.iter_mut()).take(PROOF_X_COUNT) {
        let block_index_bits = *x as u128 * k as u128;
        let block_index = (block_index_bits / K_F1_BLOCK_SIZE_BITS as u128) as u64;
        let prefix_bits = (block_index_bits % K_F1_BLOCK_SIZE_BITS as u128) as u32;
        let first_block_bits = min(u32::from(K_F1_BLOCK_SIZE_BITS) - prefix_bits, k);
        blocks.clear();
        chacha8_get_keystream(&enc_ctx, block_index, 1, &mut blocks);
        let first_block = BitReader::from_bytes_be(&blocks, K_F1_BLOCK_SIZE_BITS as usize);
        let mut output_bits = if first_block_bits < k {
            blocks.clear();
            chacha8_get_keystream(&enc_ctx, block_index + 1, 1, &mut blocks);
            let second_block = BitReader::from_bytes_be(&blocks, K_F1_BLOCK_SIZE_BITS as usize);
            first_block.slice(prefix_bits as usize)
                + second_block.range(0, (prefix_bits + k) as usize)
        } else {
            first_block.range(prefix_bits as usize, (prefix_bits + k) as usize)
        };
        let mut y = output_bits.read_u64(k as usize)?;
        y = (y << K_EXTRA_BITS) | (*x >> x_shift);
        *fx = y;
        meta.push(BitReader::new(*x, k as usize));
    }
    Ok(())
}

pub fn forward_prop_f1_to_f7(
    mut proof: Option<&mut Vec<u64>>,
    fx: &mut [u64],
    meta: &mut [BitReader],
    k: u32,
) -> Result<(), Error> {
    let mut iter_count = PROOF_X_COUNT;
    for table in [
        PlotTable::Table2,
        PlotTable::Table3,
        PlotTable::Table4,
        PlotTable::Table5,
        PlotTable::Table6,
        PlotTable::Table7,
    ] {
        let mut i = 0;
        let mut dst = 0;
        let mut tmp_meta = BitReader::default();
        while i < iter_count {
            let mut y0 = fx[i];
            let mut y1 = fx[i + 1];
            let mut l_meta = &meta[i];
            let mut r_meta = &meta[i + 1];
            if y0 > y1 {
                swap(&mut y0, &mut y1);
                swap(&mut l_meta, &mut r_meta);
                if let Some(proof) = &mut proof {
                    let count = 1 << (table as usize - 1);
                    let (x, x_next) = (*proof)[i * count..].split_at_mut(count);
                    x.swap_with_slice(&mut x_next[0..count]);
                }
            }
            // Must be on the same group
            if !fx_match(&y0, &y1) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Values Not in Same Group",
                ));
            }
            // FxGen
            fx_gen(table, k, y0, l_meta, r_meta, &mut fx[dst], &mut tmp_meta)?;
            meta[dst] = tmp_meta;
            tmp_meta = BitReader::default();
            i += 2;
            dst += 1;
        }
        iter_count >>= 1;
    }
    Ok(())
}

#[allow(clippy::cast_sign_loss)]
pub fn generate_fx_for_pairs_table2(
    thread_count: usize,
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta1>,
    y_out: Span<u64>,
    meta_out: Span<K32Meta2>,
) {
    debug_assert!(y_out.len() >= pairs.len());
    debug_assert!(meta_out.len() >= pairs.len());
    if thread_count == 1 {
        generate_fx_table2(k, pairs, y_in, meta_in, y_out, meta_out);
    } else {
        (0..thread_count)
            .collect::<Vec<usize>>()
            .into_par_iter()
            .for_each(|i| {
                let t_info = calc_thread_vars(i, thread_count, pairs.len() as usize);
                let pairs = pairs.range(t_info.offset, t_info.count);
                let y_out = y_out.range(t_info.offset, t_info.count);
                let meta_out = meta_out.range(t_info.offset, t_info.count);
                generate_fx_table2(k, pairs, y_in, meta_in, y_out, meta_out);
            });
    }
}

fn generate_fx_table2(
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta1>,
    mut y_out: Span<u64>,
    mut meta_out: Span<K32Meta2>,
) {
    let y_shift = 64 - (k + K_EXTRA_BITS);
    let buffer_size = ucdiv_t(
        (k + K_EXTRA_BITS) as usize + k as usize * get_meta_in(PlotTable::Table2).multiplier * 2,
        8,
    );
    let mut input: [u8; 16] = [0; 16];
    let mut u64_buffer: [u8; 8] = [0; 8];
    let mut hasher = Hasher::new();
    for (pair, (y_out, meta_out)) in pairs[0..pairs.len()].iter().zip(
        y_out[0..pairs.len()]
            .iter_mut()
            .zip(meta_out[0..pairs.len()].iter_mut()),
    ) {
        let l = u64::from(meta_in[pair.left as usize]);
        let r = u64::from(meta_in[pair.right as usize]);
        input[0..8].copy_from_slice(&((y_in[pair.left as usize] << 26) | (l >> 6)).to_be_bytes());
        input[8..16].copy_from_slice(&((l << 58) | (r << 26)).to_be_bytes());
        *meta_out = (l << 32) | r;
        hasher.reset();
        hasher.update(&input[0..buffer_size]);
        u64_buffer.copy_from_slice(&hasher.finalize().as_bytes()[0..8]);
        *y_out = u64::from_be_bytes(u64_buffer) >> y_shift;
    }
}

#[allow(clippy::cast_sign_loss)]
pub fn generate_fx_for_pairs_table3(
    k: u8,
    thread_count: usize,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta2>,
    y_out: Span<u64>,
    meta_out: Span<K32Meta4>,
) {
    assert!(y_out.len() >= pairs.len());
    assert!(meta_out.len() >= pairs.len());
    if thread_count == 1 {
        generate_fx_table3(k, pairs, y_in, meta_in, y_out, meta_out);
    } else {
        (0..thread_count)
            .collect::<Vec<usize>>()
            .into_par_iter()
            .for_each(|i| {
                let t_info = calc_thread_vars(i, thread_count, pairs.len() as usize);
                let pairs = pairs.range(t_info.offset, t_info.count);
                let y_out = y_out.range(t_info.offset, t_info.count);
                let meta_out = meta_out.range(t_info.offset, t_info.count);
                generate_fx_table3(k, pairs, y_in, meta_in, y_out, meta_out);
            });
    }
}

fn generate_fx_table3(
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta2>,
    mut y_out: Span<u64>,
    mut meta_out: Span<K32Meta4>,
) {
    let y_shift = 64 - (k + K_EXTRA_BITS);
    let buffer_size = ucdiv_t(
        (k + K_EXTRA_BITS) as usize + k as usize * get_meta_in(PlotTable::Table3).multiplier * 2,
        8,
    );
    let mut input: [u8; 24] = [0; 24];
    let mut u64_buffer: [u8; 8] = [0; 8];
    let mut hasher = Hasher::new();
    for (pair, (y_out, meta_out)) in pairs[0..pairs.len()].iter().zip(
        y_out[0..pairs.len()]
            .iter_mut()
            .zip(meta_out[0..pairs.len()].iter_mut()),
    ) {
        let l = &meta_in[pair.left as usize];
        let r = &meta_in[pair.right as usize];
        input[0..8].copy_from_slice(&((y_in[pair.left as usize] << 26) | (l >> 38)).to_be_bytes());
        input[8..16].copy_from_slice(&((l << 26) | (r >> 38)).to_be_bytes());
        input[16..24].copy_from_slice(&(r << 26).to_be_bytes());
        meta_out.m0 = *l;
        meta_out.m1 = *r;
        hasher.reset();
        hasher.update(&input[0..buffer_size]);
        u64_buffer.copy_from_slice(&hasher.finalize().as_bytes()[0..8]);
        *y_out = u64::from_be_bytes(u64_buffer) >> y_shift;
    }
}

#[allow(clippy::cast_sign_loss)]
pub fn generate_fx_for_pairs_table4(
    k: u8,
    thread_count: usize,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta4>,
    y_out: Span<u64>,
    meta_out: Span<K32Meta4>,
) {
    assert!(y_out.len() >= pairs.len());
    assert!(meta_out.len() >= pairs.len());
    if thread_count == 1 {
        generate_fx_table4(k, pairs, y_in, meta_in, y_out, meta_out);
    } else {
        (0..thread_count)
            .collect::<Vec<usize>>()
            .into_par_iter()
            .for_each(|i| {
                let t_info = calc_thread_vars(i, thread_count, pairs.len() as usize);
                let pairs = pairs.range(t_info.offset, t_info.count);
                let y_out = y_out.range(t_info.offset, t_info.count);
                let meta_out = meta_out.range(t_info.offset, t_info.count);
                generate_fx_table4(k, pairs, y_in, meta_in, y_out, meta_out);
            });
    }
}

fn generate_fx_table4(
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta4>,
    mut y_out: Span<u64>,
    mut meta_out: Span<K32Meta4>,
) {
    let y_size = k + K_EXTRA_BITS;
    let y_shift = 64 - (k + K_EXTRA_BITS);
    let buffer_size = ucdiv_t(
        y_size as usize + ((k as usize * get_meta_in(PlotTable::Table4).multiplier) * 2),
        8,
    );
    let mut input: [u8; 40] = [0; 40];
    let mut u64_buffer: [u8; 8] = [0; 8];
    let mut hasher = Hasher::new();
    for (pair, (y_out, meta_out)) in pairs[0..pairs.len()].iter().zip(
        y_out[0..pairs.len()]
            .iter_mut()
            .zip(meta_out[0..pairs.len()].iter_mut()),
    ) {
        let l = &meta_in[pair.left as usize];
        let r = &meta_in[pair.right as usize];
        input[0..8]
            .copy_from_slice(&((y_in[pair.left as usize] << 26) | (l.m0 >> 38)).to_be_bytes());
        input[8..16].copy_from_slice(&((l.m0 << 26) | (l.m1 >> 38)).to_be_bytes());
        input[16..24].copy_from_slice(&((l.m1 << 26) | (r.m0 >> 38)).to_be_bytes());
        input[24..32].copy_from_slice(&((r.m0 << 26) | (r.m1 >> 38)).to_be_bytes());
        input[32..40].copy_from_slice(&(r.m1 << 26).to_be_bytes());
        hasher.reset();
        hasher.update(&input[0..buffer_size]);
        let output = hasher.finalize();
        let output = output.as_bytes();
        u64_buffer.copy_from_slice(&output[0..8]);
        let o2 = u64::from_be_bytes(u64_buffer);
        *y_out = o2 >> y_shift;
        u64_buffer.copy_from_slice(&output[8..16]);
        let h1 = u64::from_be_bytes(u64_buffer);
        u64_buffer.copy_from_slice(&output[16..24]);
        let h2 = u64::from_be_bytes(u64_buffer);
        meta_out.m0 = (o2 << y_size) | (h1 >> 26);
        meta_out.m1 = (h1 << 38) | (h2 >> 26);
    }
}

#[allow(clippy::cast_sign_loss)]
pub fn generate_fx_for_pairs_table5(
    k: u8,
    thread_count: usize,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta4>,
    y_out: Span<u64>,
    meta_out: Span<K32Meta3>,
) {
    assert!(y_out.len() >= pairs.len());
    assert!(meta_out.len() >= pairs.len());
    if thread_count == 1 {
        generate_fx_table5(k, pairs, y_in, meta_in, y_out, meta_out);
    } else {
        (0..thread_count)
            .collect::<Vec<usize>>()
            .into_par_iter()
            .for_each(|i| {
                let t_info = calc_thread_vars(i, thread_count, pairs.len() as usize);
                let pairs = pairs.range(t_info.offset, t_info.count);
                let y_out = y_out.range(t_info.offset, t_info.count);
                let meta_out = meta_out.range(t_info.offset, t_info.count);
                generate_fx_table5(k, pairs, y_in, meta_in, y_out, meta_out);
            });
    }
}

fn generate_fx_table5(
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta4>,
    mut y_out: Span<u64>,
    mut meta_out: Span<K32Meta3>,
) {
    let y_size = k + K_EXTRA_BITS;
    let y_shift = 64 - (k + K_EXTRA_BITS);
    let buffer_size = ucdiv_t(
        y_size as usize + k as usize * get_meta_in(PlotTable::Table5).multiplier * 2,
        8,
    );
    let mut input: [u8; 40] = [0; 40];
    let mut u64_buffer: [u8; 8] = [0; 8];
    let mut hasher = Hasher::new();
    for (pair, (y_out, meta_out)) in pairs[0..pairs.len()].iter().zip(
        y_out[0..pairs.len()]
            .iter_mut()
            .zip(meta_out[0..pairs.len()].iter_mut()),
    ) {
        let l = &meta_in[pair.left as usize];
        let r = &meta_in[pair.right as usize];
        input[0..8]
            .copy_from_slice(&((y_in[pair.left as usize] << 26) | (l.m0 >> 38)).to_be_bytes());
        input[8..16].copy_from_slice(&((l.m0 << 26) | (l.m1 >> 38)).to_be_bytes());
        input[16..24].copy_from_slice(&((l.m1 << 26) | (r.m0 >> 38)).to_be_bytes());
        input[24..32].copy_from_slice(&((r.m0 << 26) | (r.m1 >> 38)).to_be_bytes());
        input[32..40].copy_from_slice(&(r.m1 << 26).to_be_bytes());
        hasher.reset();
        hasher.update(&input[0..buffer_size]);
        let output = hasher.finalize();
        let output = output.as_bytes();
        u64_buffer.copy_from_slice(&output[0..8]);
        let o2 = u64::from_be_bytes(u64_buffer);
        *y_out = o2 >> y_shift;
        u64_buffer.copy_from_slice(&output[8..16]);
        let h1 = u64::from_be_bytes(u64_buffer);
        u64_buffer.copy_from_slice(&output[16..24]);
        let h2 = u64::from_be_bytes(u64_buffer);
        meta_out.m0 = (o2 << y_size) | (h1 >> 26);
        meta_out.m1 = ((h1 << 6) & 0xFFFF_FFC0) | (h2 >> 58);
    }
}

#[allow(clippy::cast_sign_loss)]
pub fn generate_fx_for_pairs_table6(
    k: u8,
    thread_count: usize,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta3>,
    y_out: Span<u64>,
    meta_out: Span<K32Meta2>,
) {
    assert!(y_out.len() >= pairs.len());
    assert!(meta_out.len() >= pairs.len());
    if thread_count == 1 {
        generate_fx_table6(k, pairs, y_in, meta_in, y_out, meta_out);
    } else {
        (0..thread_count)
            .collect::<Vec<usize>>()
            .into_par_iter()
            .for_each(|i| {
                let t_info = calc_thread_vars(i, thread_count, pairs.len() as usize);
                let pairs = pairs.range(t_info.offset, t_info.count);
                let y_out = y_out.range(t_info.offset, t_info.count);
                let meta_out = meta_out.range(t_info.offset, t_info.count);
                generate_fx_table6(k, pairs, y_in, meta_in, y_out, meta_out);
            });
    }
}

fn generate_fx_table6(
    k: u8,
    pairs: Span<Pair>,
    y_in: Span<u64>,
    meta_in: Span<K32Meta3>,
    mut y_out: Span<u64>,
    mut meta_out: Span<K32Meta2>,
) {
    let y_size = k + K_EXTRA_BITS;
    let y_shift = 64 - (k + K_EXTRA_BITS);
    let buffer_size = ucdiv_t(
        y_size as usize + k as usize * get_meta_in(PlotTable::Table6).multiplier * 2,
        8,
    );
    let mut input: [u8; 32] = [0; 32];
    let mut u64_buffer: [u8; 8] = [0; 8];
    let mut hasher = Hasher::new();
    for (pair, (y_out, meta_out)) in pairs[0..pairs.len()].iter().zip(
        y_out[0..pairs.len()]
            .iter_mut()
            .zip(meta_out[0..pairs.len()].iter_mut()),
    ) {
        let l0 = &meta_in[pair.left as usize].m0;
        let l1 = &meta_in[pair.left as usize].m1 & 0xFFFF_FFFF;
        let r0 = &meta_in[pair.right as usize].m0;
        let r1 = &meta_in[pair.right as usize].m1 & 0xFFFF_FFFF;
        input[0..8].copy_from_slice(&((y_in[pair.left as usize] << 26) | (l0 >> 38)).to_be_bytes());
        input[8..16].copy_from_slice(&((l0 << 26) | (l1 >> 6)).to_be_bytes());
        input[16..24].copy_from_slice(&((l1 << 58) | (r0 >> 6)).to_be_bytes());
        input[24..32].copy_from_slice(&((r0 << 58) | (r1 << 26)).to_be_bytes());
        hasher.reset();
        hasher.update(&input[0..buffer_size]);
        let output = hasher.finalize();
        let output = output.as_bytes();
        u64_buffer.copy_from_slice(&output[0..8]);
        let o2 = u64::from_be_bytes(u64_buffer);
        *y_out = o2 >> y_shift;
        u64_buffer.copy_from_slice(&output[8..16]);
        let h1 = u64::from_be_bytes(u64_buffer);
        *meta_out = (o2 << y_size) | (h1 >> 26);
    }
}

#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn fx_match(y_l: &u64, y_r: &u64) -> bool {
    let y_l = *y_l as usize;
    let y_r = *y_r as usize;
    let group_l = y_l / K_BC;
    let group_r = y_r / K_BC;
    if group_r - group_l != 1 {
        return false;
    }
    let local_ry = (y_r - group_r * K_BC) as u16;
    L_TARGETS[group_l & 1][y_l - group_l * K_BC].contains(&local_ry)
}

pub fn fx_gen(
    table: PlotTable,
    k: u32,
    y: u64,
    l_meta: &BitReader,
    r_meta: &BitReader,
    out_y: &mut u64,
    out_meta: &mut BitReader,
) -> Result<(), Error> {
    let mut input = BitReader::new(y, k as usize + K_EXTRA_BITS as usize);
    if table < PlotTable::Table4 {
        *out_meta += l_meta;
        *out_meta += r_meta;
        input += &*out_meta;
    } else {
        input += l_meta;
        input += r_meta;
    }
    let mut hasher = Hasher::new();
    hasher.update(&input.to_bytes());
    let hash_bytes = hasher.finalize();
    let y_bits = k as usize + K_EXTRA_BITS as usize;
    *out_y = bytes_to_u64(hash_bytes.as_bytes()) >> (64 - y_bits);
    if table >= PlotTable::Table4 && table < PlotTable::Table7 {
        let start_byte = y_bits / 8;
        let start_bit = y_bits - start_byte * 8;
        *out_meta = BitReader::from_bytes_be_offset(
            &hash_bytes.as_bytes()[start_byte..],
            k as usize * get_meta_out(table).multiplier,
            start_bit,
        );
    }
    Ok(())
}
