use crate::bits::compact_bits;
use crate::chainer::Chain;
use crate::compute::{BATCH_SIZE, CpuHasher, HashEngine, SCRATCH_BYTES, Work, allocate, config};
use crate::core::ProofCore;
use crate::device::{self, Record};
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use crate::validator::ProofValidator;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;

const CPU_SCAN_BATCH_SIZE: usize = 262_144;
const CPU_SCAN_CHUNK_SIZE: usize = 4096;

#[derive(Clone, Copy)]
struct Target {
    info: u32,
    left: u32,
}

#[derive(Clone, Copy)]
struct Pair {
    meta: u64,
    info: u32,
    prefix: u32,
}

#[derive(Clone, Copy)]
struct Quad {
    meta: u64,
    info: u32,
    x_bits: u32,
    xs: [u32; 4],
}

pub fn solve(
    params: &ProofParams,
    chain: &Chain,
    challenge: Bytes32,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    solve_with_engine(
        params,
        chain,
        challenge,
        limits,
        cancelled,
        &mut CpuHasher::new(params),
    )
}

pub fn solve_with_engine(
    params: &ProofParams,
    chain: &Chain,
    challenge: Bytes32,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    engine: &mut impl HashEngine,
) -> Result<Vec<u8>, Error> {
    let core = ProofCore::new(params.clone())?;
    if !crate::chainer::Chainer::new(&core, challenge)
        .validate(chain, &core.select_challenge_sets(challenge).ranges)
    {
        return Err(Error::new(ErrorKind::InvalidInput, "invalid quality chain"));
    }
    let mut prefixes = Vec::with_capacity(64);
    for fragment in chain.fragments {
        prefixes.extend_from_slice(&core.fragment_codec.x_bits(fragment));
    }
    prefixes.sort_unstable();
    prefixes.dedup();
    let half = u32::from(params.k()) / 2;
    let width = 1usize << half;
    let left_count = prefixes
        .len()
        .checked_mul(width)
        .ok_or_else(|| Error::other("solver candidate overflow"))?;
    let target_count = left_count
        .checked_mul(4)
        .ok_or_else(|| Error::other("solver target overflow"))?;
    if target_count > limits.max_entries {
        return Err(Error::other("solver target entry budget exceeded"));
    }
    let minimum_work = (1u64 << params.k())
        + left_count as u64
        + target_count as u64 * (1u64 << (params.strength() - 2));
    if limits.max_work < minimum_work {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "solver work budget is below the required generation and target hashing",
        ));
    }
    let bitmap_bytes = (1u64 << params.k()).div_ceil(8);
    let index_bytes = ((1u64 << params.k().min(16)) + 1) * 4;
    let required = (target_count as u64)
        .checked_mul(size_of::<Target>() as u64)
        .and_then(|size| size.checked_add(left_count as u64 * 4))
        .and_then(|size| size.checked_add(bitmap_bytes))
        .and_then(|size| size.checked_add(index_bytes))
        .and_then(|size| size.checked_add(SCRATCH_BYTES))
        .ok_or_else(|| Error::other("solver memory overflow"))?;
    if required > limits.memory_bytes {
        return Err(Error::other(format!(
            "solver needs at least {required} managed bytes for these limits"
        )));
    }
    let configuration = config(params);
    let mut work = Work::new(limits, cancelled);
    let scan_batch = if engine.cpu_hasher().is_some() {
        CPU_SCAN_BATCH_SIZE
    } else {
        BATCH_SIZE
    };
    let mut inputs = allocate(BATCH_SIZE)?;
    let mut left_infos = allocate(left_count)?;
    let left_at =
        |position: usize| (prefixes[position / width] << half) | (position % width) as u32;
    for start in (0..left_count).step_by(BATCH_SIZE) {
        inputs.clear();
        for position in start..(start + BATCH_SIZE).min(left_count) {
            inputs.push([
                left_at(position) ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                0,
                0,
                0,
            ]);
        }
        for lanes in work.hash(engine, &inputs, 16)? {
            left_infos.push(lanes[0] & (u32::MAX >> (32 - params.k())));
        }
    }
    let mut targets = allocate(target_count)?;
    let mut bitmap = allocate(
        usize::try_from(bitmap_bytes)
            .map_err(|_| Error::other("solver bitmap exceeds address space"))?,
    )?;
    bitmap.resize(bitmap_bytes as usize, 0u8);
    let rounds = 16 << (u32::from(params.strength()) - 2);
    for start in (0..target_count).step_by(BATCH_SIZE) {
        inputs.clear();
        let end = (start + BATCH_SIZE).min(target_count);
        for position in start..end {
            inputs.push([1, (position % 4) as u32, left_at(position / 4), 0]);
        }
        for (position, lanes) in (start..end).zip(work.hash(engine, &inputs, rounds)?) {
            let left = left_at(position / 4);
            let info = device::target_from_hash(
                configuration,
                1,
                Record {
                    meta: u64::from(left),
                    info: left_infos[position / 4],
                    ..Record::default()
                },
                (position % 4) as u32,
                lanes[0],
            );
            bitmap[info as usize / 8] |= 1 << (info % 8);
            targets.push(Target { info, left });
        }
    }
    drop(left_infos);
    targets.par_sort_unstable_by_key(|target| (target.info, target.left));
    work.charge(0)?;
    let target_index = TargetIndex::new(&targets, params.k())?;
    // max_entries is a safety ceiling, not a reservation. Grow only the recovered
    // candidates, charging capacity (including reallocations) against live memory.
    let pair_bytes = limits.memory_bytes - required;
    let mut pairs = Vec::<Pair>::new();
    let mut pending = allocate::<(u32, u32)>(BATCH_SIZE)?;
    let flush = |pending: &mut Vec<(u32, u32)>,
                 pairs: &mut Vec<Pair>,
                 work: &mut Work<'_>,
                 engine: &mut _|
     -> Result<(), Error> {
        if pending.is_empty() {
            return Ok(());
        }
        let mut values = allocate(pending.len())?;
        values.extend(pending.iter().map(|(left, right)| [*left, 0, *right, 0]));
        for ((left, right), lanes) in pending.drain(..).zip(work.hash(engine, &values, rounds)?) {
            if lanes[3] & 3 != 0 {
                continue;
            }
            push_bounded(
                pairs,
                Pair {
                    meta: (u64::from(left) << params.k()) | u64::from(right),
                    info: lanes[0] & (u32::MAX >> (32 - params.k())),
                    prefix: left >> half,
                },
                limits.max_entries,
                pair_bytes,
            )?;
        }
        Ok(())
    };
    let initial = 1u64 << params.k();
    for start in (0..initial).step_by(scan_batch) {
        let end = (start + scan_batch as u64).min(initial);
        // Fuse CPU generation, hashing and matching in worker-local buffers.
        // Accelerated engines retain their ordinary batched hash path below.
        let chunks = if let Some(cpu) = engine.cpu_hasher() {
            let count = (end - start) as usize;
            work.charge(count as u64)?;
            (0..count)
                .into_par_iter()
                .step_by(CPU_SCAN_CHUNK_SIZE)
                .with_min_len(4)
                .map_init(
                    || (Vec::<[u32; 4]>::new(), Vec::<[u32; 4]>::new()),
                    |(inputs, hashes), chunk| {
                        let length = (count - chunk).min(CPU_SCAN_CHUNK_SIZE);
                        inputs.clear();
                        if inputs.capacity() < length {
                            inputs
                                .try_reserve_exact(length)
                                .map_err(|_| Error::other("PoS2 allocation failed"))?;
                        }
                        if hashes.capacity() < length {
                            hashes
                                .try_reserve_exact(length.saturating_sub(hashes.len()))
                                .map_err(|_| Error::other("PoS2 allocation failed"))?;
                        }
                        let offset = start + chunk as u64;
                        for right in offset..offset + length as u64 {
                            inputs.push([
                                right as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                                0,
                                0,
                                0,
                            ]);
                        }
                        hashes.resize(length, [0; 4]);
                        cpu.hash_into_serial(inputs, 16, hashes, cancelled)?;
                        match match_chunk(
                            hashes,
                            &targets,
                            &target_index,
                            &bitmap,
                            offset,
                            params.k(),
                            cancelled,
                        )? {
                            Some(matches) => Ok(ScanChunk::Sparse(matches)),
                            None => {
                                let mut retained = allocate(hashes.len())?;
                                retained.extend_from_slice(hashes);
                                Ok(ScanChunk::Dense {
                                    hashes: retained,
                                    offset,
                                })
                            }
                        }
                    },
                )
                .collect::<Result<Vec<_>, Error>>()?
        } else {
            inputs.clear();
            for right in start..end {
                inputs.push([
                    right as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                    0,
                    0,
                    0,
                ]);
            }
            let hashes = work.hash(engine, &inputs, 16)?;
            match_candidates(
                &hashes,
                &targets,
                &target_index,
                &bitmap,
                start,
                params.k(),
                cancelled,
            )?
        };
        for chunk in chunks {
            match chunk {
                ScanChunk::Sparse(matches) => {
                    for candidate in matches {
                        pending.push(candidate);
                        if pending.len() == BATCH_SIZE {
                            flush(&mut pending, &mut pairs, &mut work, engine)?;
                        }
                    }
                }
                ScanChunk::Dense { hashes, offset } => {
                    // Dense chunks retain their already computed hashes. Stream their
                    // matches without unbounded scratch or repeating the hash work.
                    for (right, lanes) in hashes.into_iter().enumerate() {
                        let info = lanes[0] & (u32::MAX >> (32 - params.k()));
                        if bitmap[info as usize / 8] & (1 << (info % 8)) == 0 {
                            continue;
                        }
                        let group = target_index.group(&targets, info);
                        let first = group.partition_point(|target| target.info < info);
                        for target in group[first..]
                            .iter()
                            .take_while(|target| target.info == info)
                        {
                            pending.push((target.left, (offset + right as u64) as u32));
                            if pending.len() == BATCH_SIZE {
                                flush(&mut pending, &mut pairs, &mut work, engine)?;
                            }
                        }
                    }
                }
            }
        }
    }
    flush(&mut pending, &mut pairs, &mut work, engine)?;
    drop(targets);
    drop(bitmap);
    drop(target_index);
    pairs.par_sort_unstable_by_key(|pair| (pair.prefix, pair.info, pair.meta));
    work.charge(0)?;
    // Targets, bitmap and left infos are no longer live during fragment recovery.
    let quad_bytes = limits
        .memory_bytes
        .checked_sub(SCRATCH_BYTES + capacity_bytes(&pairs)?)
        .ok_or_else(|| Error::other("solver memory budget exceeded"))?;
    let mut proof = [0u32; 128];
    for (index, fragment) in chain.fragments.iter().copied().enumerate() {
        let prefixes = core.fragment_codec.x_bits(fragment);
        let group = |prefix| {
            let first = pairs.partition_point(|pair| pair.prefix < prefix);
            let last = pairs.partition_point(|pair| pair.prefix <= prefix);
            &pairs[first..last]
        };
        let left = quads(
            &core,
            group(prefixes[0]),
            group(prefixes[1]),
            limits.max_entries,
            quad_bytes,
            &mut work,
        )?;
        let right = quads(
            &core,
            group(prefixes[2]),
            group(prefixes[3]),
            limits.max_entries,
            quad_bytes
                .checked_sub(capacity_bytes(&left)?)
                .ok_or_else(|| Error::other("solver memory budget exceeded"))?,
            &mut work,
        )?;
        let mut found = None;
        'search: for left in &left {
            for key in 0..params.num_match_keys(3) {
                work.charge(1)?;
                let info = device::target_from_hash(
                    configuration,
                    3,
                    Record {
                        meta: left.meta,
                        info: left.info,
                        ..Record::default()
                    },
                    key as u32,
                    core.matching_target(3, left.meta, key as u32),
                );
                let first = right.partition_point(|right| right.info < info);
                for right in right[first..].iter().take_while(|right| right.info == info) {
                    work.charge(1)?;
                    if core
                        .pairing_t3(left.meta, right.meta, left.x_bits, right.x_bits)
                        .is_some_and(|pair| pair.proof_fragment == fragment)
                    {
                        let mut values = [0; 8];
                        values[..4].copy_from_slice(&left.xs);
                        values[4..].copy_from_slice(&right.xs);
                        found = Some(values);
                        break 'search;
                    }
                }
            }
        }
        let values = found.ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                "stored fragment has no valid proof in this hash domain",
            )
        })?;
        proof[index * 8..index * 8 + 8].copy_from_slice(&values);
    }
    work.charge(0)?;
    if ProofValidator::new(params.clone())?.validate_full_proof(&proof, challenge)
        != Some(chain.fragments)
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "solved proof failed independent verification",
        ));
    }
    Ok(compact_bits(&proof, params.k()))
}

fn quads(
    core: &ProofCore,
    left: &[Pair],
    right: &[Pair],
    maximum: usize,
    memory_bytes: u64,
    work: &mut Work<'_>,
) -> Result<Vec<Quad>, Error> {
    let params = core.params();
    let configuration = config(params);
    let mask = u64::MAX >> (64 - params.k());
    let mut output = Vec::new();
    for left in left {
        for key in 0..params.num_match_keys(2) {
            work.charge(1)?;
            let info = device::target_from_hash(
                configuration,
                2,
                Record {
                    meta: left.meta,
                    info: left.info,
                    ..Record::default()
                },
                key as u32,
                core.matching_target(2, left.meta, key as u32),
            );
            let first = right.partition_point(|right| right.info < info);
            for right in right[first..].iter().take_while(|right| right.info == info) {
                work.charge(1)?;
                if let Some(pair) = core.pairing_t2(left.meta, right.meta) {
                    push_bounded(
                        &mut output,
                        Quad {
                            meta: pair.meta,
                            info: pair.match_info,
                            x_bits: pair.x_bits,
                            xs: [
                                (left.meta >> params.k()) as u32,
                                (left.meta & mask) as u32,
                                (right.meta >> params.k()) as u32,
                                (right.meta & mask) as u32,
                            ],
                        },
                        maximum,
                        memory_bytes,
                    )?;
                }
            }
        }
    }
    output.sort_unstable_by_key(|quad| (quad.info, quad.meta, quad.xs));
    Ok(output)
}

/// A small prefix directory keeps each target search inside adjacent cache lines.
struct TargetIndex {
    offsets: Vec<u32>,
    shift: u8,
}

impl TargetIndex {
    fn new(targets: &[Target], k: u8) -> Result<Self, Error> {
        let bits = k.min(16);
        let count = 1usize << bits;
        let shift = k - bits;
        let mut offsets = allocate(count + 1)?;
        let mut position = 0usize;
        for bucket in 0..=count {
            while position < targets.len() && (targets[position].info >> shift) < bucket as u32 {
                position += 1;
            }
            offsets.push(
                u32::try_from(position)
                    .map_err(|_| Error::other("solver target index overflow"))?,
            );
        }
        Ok(Self { offsets, shift })
    }

    fn group<'a>(&self, targets: &'a [Target], info: u32) -> &'a [Target] {
        let bucket = (info >> self.shift) as usize;
        &targets[self.offsets[bucket] as usize..self.offsets[bucket + 1] as usize]
    }
}

type MatchBatch = Vec<(u32, u32)>;

enum ScanChunk {
    Sparse(MatchBatch),
    Dense { hashes: Vec<[u32; 4]>, offset: u64 },
}

fn match_candidates(
    hashes: &[[u32; 4]],
    targets: &[Target],
    target_index: &TargetIndex,
    bitmap: &[u8],
    start: u64,
    k: u8,
    cancelled: &AtomicBool,
) -> Result<Vec<ScanChunk>, Error> {
    hashes
        .par_chunks(4096)
        .enumerate()
        .map(|(chunk, hashes)| {
            let offset = start + (chunk * 4096) as u64;
            match match_chunk(hashes, targets, target_index, bitmap, offset, k, cancelled)? {
                Some(matches) => Ok(ScanChunk::Sparse(matches)),
                None => {
                    let mut retained = allocate(hashes.len())?;
                    retained.extend_from_slice(hashes);
                    Ok(ScanChunk::Dense {
                        hashes: retained,
                        offset,
                    })
                }
            }
        })
        .collect()
}

fn match_chunk(
    hashes: &[[u32; 4]],
    targets: &[Target],
    target_index: &TargetIndex,
    bitmap: &[u8],
    start: u64,
    k: u8,
    cancelled: &AtomicBool,
) -> Result<Option<MatchBatch>, Error> {
    let mut matches = allocate::<(u32, u32)>(4096)?;
    let selected = select_infos(hashes, bitmap, k, cancelled)?;
    for (offset, info) in selected {
        let right = start + u64::from(offset);
        let group = target_index.group(targets, info);
        let first = group.partition_point(|target| target.info < info);
        for target in group[first..]
            .iter()
            .take_while(|target| target.info == info)
        {
            if matches.len() == 4096 {
                return Ok(None);
            }
            matches.push((target.left, right as u32));
        }
    }
    Ok(Some(matches))
}

fn select_infos(
    hashes: &[[u32; 4]],
    bitmap: &[u8],
    k: u8,
    cancelled: &AtomicBool,
) -> Result<Vec<(u32, u32)>, Error> {
    crate::compute::check_cancelled(cancelled)?;
    let mut selected = allocate(hashes.len())?;
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2")
        && bitmap.len() >= 4
        && bitmap.len() >= ((1u64 << k).div_ceil(8)) as usize
    {
        // All gathered bitmap words are in bounds after masking to k bits.
        unsafe { select_infos_avx2(hashes, bitmap, k, &mut selected) };
        crate::compute::check_cancelled(cancelled)?;
        return Ok(selected);
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    if std::arch::is_aarch64_feature_detected!("neon")
        && bitmap.len() >= 4
        && bitmap.len() >= ((1u64 << k).div_ceil(8)) as usize
    {
        // NEON handles four hashes; the bounded scalar loads replace x86 gathers.
        unsafe { select_infos_neon(hashes, bitmap, k, &mut selected) };
        crate::compute::check_cancelled(cancelled)?;
        return Ok(selected);
    }
    select_infos_scalar(hashes, bitmap, k, 0, &mut selected);
    crate::compute::check_cancelled(cancelled)?;
    Ok(selected)
}

fn select_infos_scalar(
    hashes: &[[u32; 4]],
    bitmap: &[u8],
    k: u8,
    start: usize,
    selected: &mut Vec<(u32, u32)>,
) {
    for (offset, lanes) in hashes.iter().enumerate() {
        let info = lanes[0] & (u32::MAX >> (32 - k));
        if bitmap[info as usize / 8] & (1 << (info % 8)) != 0 {
            selected.push(((start + offset) as u32, info));
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn select_infos_avx2(
    hashes: &[[u32; 4]],
    bitmap: &[u8],
    k: u8,
    selected: &mut Vec<(u32, u32)>,
) {
    use std::arch::x86_64::*;
    unsafe {
        let indices = _mm256_setr_epi32(0, 4, 8, 12, 16, 20, 24, 28);
        let info_mask = _mm256_set1_epi32((u32::MAX >> (32 - k)) as i32);
        let shifts_mask = _mm256_set1_epi32(31);
        let (chunks, tail) = hashes.as_chunks::<8>();
        for (chunk, hashes) in chunks.iter().enumerate() {
            let infos = _mm256_and_si256(
                _mm256_i32gather_epi32::<4>(hashes.as_ptr().cast(), indices),
                info_mask,
            );
            let words =
                _mm256_i32gather_epi32::<4>(bitmap.as_ptr().cast(), _mm256_srli_epi32::<5>(infos));
            let hits = _mm256_srlv_epi32(words, _mm256_and_si256(infos, shifts_mask));
            let mut mask =
                _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_slli_epi32::<31>(hits))) as u32;
            let mut lanes = [0u32; 8];
            _mm256_storeu_si256(lanes.as_mut_ptr().cast(), infos);
            while mask != 0 {
                let lane = mask.trailing_zeros() as usize;
                selected.push(((chunk * 8 + lane) as u32, lanes[lane]));
                mask &= mask - 1;
            }
        }
        select_infos_scalar(tail, bitmap, k, chunks.len() * 8, selected);
    }
}

#[cfg(all(target_arch = "aarch64", target_endian = "little"))]
#[target_feature(enable = "neon")]
unsafe fn select_infos_neon(
    hashes: &[[u32; 4]],
    bitmap: &[u8],
    k: u8,
    selected: &mut Vec<(u32, u32)>,
) {
    use std::arch::aarch64::*;
    unsafe {
        let info_mask = vdupq_n_u32(u32::MAX >> (32 - k));
        let shift_mask = vdupq_n_u32(31);
        let weights = vld1q_u32([1u32, 2, 4, 8].as_ptr());
        let (chunks, tail) = hashes.as_chunks::<4>();
        for (chunk, hashes) in chunks.iter().enumerate() {
            let infos = vandq_u32(vld4q_u32(hashes.as_ptr().cast()).0, info_mask);
            let mut lanes = [0u32; 4];
            vst1q_u32(lanes.as_mut_ptr(), infos);
            let words: [u32; 4] = std::array::from_fn(|index| {
                std::ptr::read_unaligned(
                    bitmap
                        .as_ptr()
                        .add((lanes[index] as usize >> 5) * 4)
                        .cast::<u32>(),
                )
            });
            let shifts = vnegq_s32(vreinterpretq_s32_u32(vandq_u32(infos, shift_mask)));
            let hits = vandq_u32(vshlq_u32(vld1q_u32(words.as_ptr()), shifts), vdupq_n_u32(1));
            let mut mask = vaddvq_u32(vmulq_u32(hits, weights));
            while mask != 0 {
                let lane = mask.trailing_zeros() as usize;
                selected.push(((chunk * 4 + lane) as u32, lanes[lane]));
                mask &= mask - 1;
            }
        }
        select_infos_scalar(tail, bitmap, k, chunks.len() * 4, selected);
    }
}

fn capacity_bytes<T>(values: &Vec<T>) -> Result<u64, Error> {
    (values.capacity() as u64)
        .checked_mul(size_of::<T>() as u64)
        .ok_or_else(|| Error::other("solver memory overflow"))
}

/// Bound allocated capacity, including simultaneous old/new buffers during growth.
fn push_bounded<T>(
    values: &mut Vec<T>,
    value: T,
    maximum: usize,
    memory_bytes: u64,
) -> Result<(), Error> {
    if values.len() >= maximum {
        return Err(Error::other("solver entry budget exceeded"));
    }
    if values.len() == values.capacity() {
        let old_bytes = capacity_bytes(values)?;
        let available = memory_bytes
            .checked_sub(old_bytes)
            .ok_or_else(|| Error::other("solver memory budget exceeded"))?;
        let capacity_limit = usize::try_from(available / size_of::<T>() as u64)
            .unwrap_or(usize::MAX)
            .min(maximum);
        let desired = values
            .capacity()
            .saturating_mul(2)
            .max(256)
            .min(capacity_limit);
        if desired <= values.len() {
            return Err(Error::other("solver memory budget exceeded"));
        }
        values
            .try_reserve_exact(desired - values.len())
            .map_err(|_| Error::other("PoS2 allocation failed"))?;
        if capacity_bytes(values)? > available {
            return Err(Error::other("solver memory budget exceeded"));
        }
    }
    values.push(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_bitmap_filter_matches_scalar_for_boundaries_and_tails() {
        let k = 18;
        let mut bitmap = vec![0u8; (1 << k) / 8];
        for (index, byte) in bitmap.iter_mut().enumerate() {
            *byte = (index.wrapping_mul(73) ^ (index >> 3)) as u8;
        }
        bitmap[0] = 1;
        *bitmap.last_mut().unwrap() = 128;
        for count in [0, 1, 3, 4, 7, 8, 9, 15, 16, 17, 255, 256, 4096] {
            let mut hashes = vec![[0u32; 4]; count];
            for (index, hash) in hashes.iter_mut().enumerate() {
                hash[0] = (index as u32).wrapping_mul(2_654_435_761);
            }
            if count > 1 {
                hashes[count - 1][0] = u32::MAX;
            }
            let mut expected = Vec::new();
            select_infos_scalar(&hashes, &bitmap, k, 0, &mut expected);
            assert_eq!(
                select_infos(&hashes, &bitmap, k, &AtomicBool::new(false)).unwrap(),
                expected
            );
        }
        let hashes = [[u32::MAX, 0, 0, 0]; 9];
        bitmap.fill(0);
        assert!(
            select_infos(&hashes, &bitmap, k, &AtomicBool::new(false))
                .unwrap()
                .is_empty()
        );
        bitmap.fill(255);
        assert_eq!(
            select_infos(&hashes, &bitmap, k, &AtomicBool::new(false))
                .unwrap()
                .len(),
            9
        );
        assert_eq!(
            select_infos(&hashes, &bitmap, k, &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            ErrorKind::Interrupted
        );
    }

    #[test]
    fn target_index_covers_duplicates_empty_buckets_and_last_bucket() {
        let targets = [
            Target { info: 0, left: 1 },
            Target { info: 0, left: 2 },
            Target {
                info: 65535,
                left: 3,
            },
            Target {
                info: (1 << 18) - 1,
                left: 4,
            },
        ];
        let index = TargetIndex::new(&targets, 18).unwrap();
        for info in [0, 1, 4, 65535, (1 << 18) - 1] {
            let full = targets
                .iter()
                .filter(|target| target.info == info)
                .map(|target| target.left)
                .collect::<Vec<_>>();
            let indexed = index
                .group(&targets, info)
                .iter()
                .filter(|target| target.info == info)
                .map(|target| target.left)
                .collect::<Vec<_>>();
            assert_eq!(indexed, full);
        }
    }

    #[test]
    fn parallel_matches_preserve_order_mask_and_partial_chunks() {
        let targets = [
            Target { info: 1, left: 10 },
            Target { info: 1, left: 20 },
            Target { info: 3, left: 30 },
        ];
        let mut hashes = vec![[0; 4]; 4099];
        hashes[0][0] = 257; // Upper bits must be masked to k.
        hashes[4096][0] = 3;
        hashes[4098][0] = 1;
        let matches = match_candidates(
            &hashes,
            &targets,
            &TargetIndex::new(&targets, 8).unwrap(),
            &[10],
            100,
            8,
            &AtomicBool::new(false),
        )
        .unwrap()
        .into_iter()
        .flat_map(|chunk| match chunk {
            ScanChunk::Sparse(matches) => matches,
            ScanChunk::Dense { .. } => panic!("sparse fixture unexpectedly used dense fallback"),
        })
        .collect::<Vec<_>>();
        assert_eq!(
            matches,
            [(10, 100), (20, 100), (30, 4196), (10, 4198), (20, 4198)]
        );
    }

    #[test]
    fn dense_matches_use_streaming_fallback_and_cancellation_is_preserved() {
        let targets = vec![Target { info: 1, left: 10 }; 4097];
        let hashes = [[1, 0, 0, 0]];
        assert!(
            match_chunk(
                &hashes,
                &targets,
                &TargetIndex::new(&targets, 8).unwrap(),
                &[2],
                0,
                8,
                &AtomicBool::new(false)
            )
            .unwrap()
            .is_none()
        );
        let chunks = match_candidates(
            &hashes,
            &targets,
            &TargetIndex::new(&targets, 8).unwrap(),
            &[2],
            123,
            8,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            ScanChunk::Dense {
                hashes: retained,
                offset,
            } => {
                assert_eq!(retained, &hashes);
                assert_eq!(*offset, 123);
            }
            ScanChunk::Sparse(_) => panic!("dense fixture must retain hashes for streaming"),
        }
        assert_eq!(
            match_chunk(
                &hashes,
                &targets,
                &TargetIndex::new(&targets, 8).unwrap(),
                &[2],
                0,
                8,
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .kind(),
            ErrorKind::Interrupted
        );
    }

    #[test]
    fn growth_obeys_memory_and_entry_limits() {
        let mut values = Vec::<Quad>::new();
        let value = Quad {
            meta: 0,
            info: 0,
            x_bits: 0,
            xs: [0; 4],
        };
        let bytes = 256 * size_of::<Quad>() as u64;
        for _ in 0..256 {
            push_bounded(&mut values, value, 310_000_000, bytes).unwrap();
        }
        assert_eq!(capacity_bytes(&values).unwrap(), bytes);
        assert!(push_bounded(&mut values, value, 310_000_000, bytes).is_err());
        assert_eq!(values.len(), 256);
        assert!(push_bounded(&mut values, value, 256, bytes * 4).is_err());
    }

    #[test]
    fn growth_accounts_for_old_and_new_allocations() {
        let mut values = Vec::<Pair>::new();
        let value = Pair {
            meta: 0,
            info: 0,
            prefix: 0,
        };
        let bytes = 768 * size_of::<Pair>() as u64;
        for _ in 0..257 {
            push_bounded(&mut values, value, 310_000_000, bytes).unwrap();
        }
        assert_eq!(values.capacity(), 512);
        for _ in 257..512 {
            push_bounded(&mut values, value, 310_000_000, bytes).unwrap();
        }
        assert!(push_bounded(&mut values, value, 310_000_000, bytes).is_err());
    }
}
