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
    let required = (target_count as u64)
        .checked_mul(size_of::<Target>() as u64)
        .and_then(|size| size.checked_add(left_count as u64 * 4))
        .and_then(|size| {
            (limits.max_entries as u64)
                .checked_mul((size_of::<Pair>() + 2 * size_of::<Quad>()) as u64)
                .and_then(|extra| size.checked_add(extra))
        })
        .and_then(|size| size.checked_add(bitmap_bytes))
        .and_then(|size| size.checked_add(SCRATCH_BYTES))
        .ok_or_else(|| Error::other("solver memory overflow"))?;
    if required > limits.memory_bytes {
        return Err(Error::other(format!(
            "solver needs at least {required} managed bytes for these limits"
        )));
    }
    let configuration = config(params);
    let mut work = Work::new(limits, cancelled);
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
    let mut pairs = allocate::<Pair>(limits.max_entries)?;
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
            if pairs.len() == limits.max_entries {
                return Err(Error::other("solver pairing entry budget exceeded"));
            }
            pairs.push(Pair {
                meta: (u64::from(left) << params.k()) | u64::from(right),
                info: lanes[0] & (u32::MAX >> (32 - params.k())),
                prefix: left >> half,
            });
        }
        Ok(())
    };
    let initial = 1u64 << params.k();
    for start in (0..initial).step_by(BATCH_SIZE) {
        inputs.clear();
        let end = (start + BATCH_SIZE as u64).min(initial);
        for right in start..end {
            inputs.push([
                right as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                0,
                0,
                0,
            ]);
        }
        for (right, lanes) in (start..end).zip(work.hash(engine, &inputs, 16)?) {
            let info = lanes[0] & (u32::MAX >> (32 - params.k()));
            if bitmap[info as usize / 8] & (1 << (info % 8)) == 0 {
                continue;
            }
            let first = targets.partition_point(|target| target.info < info);
            for target in targets[first..]
                .iter()
                .take_while(|target| target.info == info)
            {
                pending.push((target.left, right as u32));
                if pending.len() == BATCH_SIZE {
                    flush(&mut pending, &mut pairs, &mut work, engine)?;
                }
            }
        }
    }
    flush(&mut pending, &mut pairs, &mut work, engine)?;
    drop(targets);
    drop(bitmap);
    pairs.par_sort_unstable_by_key(|pair| (pair.prefix, pair.info, pair.meta));
    work.charge(0)?;
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
            &mut work,
        )?;
        let right = quads(
            &core,
            group(prefixes[2]),
            group(prefixes[3]),
            limits.max_entries,
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
    work: &mut Work<'_>,
) -> Result<Vec<Quad>, Error> {
    let params = core.params();
    let configuration = config(params);
    let mask = u64::MAX >> (64 - params.k());
    let mut output = allocate(maximum)?;
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
                    if output.len() == maximum {
                        return Err(Error::other("solver quad entry budget exceeded"));
                    }
                    output.push(Quad {
                        meta: pair.meta,
                        info: pair.match_info,
                        x_bits: pair.x_bits,
                        xs: [
                            (left.meta >> params.k()) as u32,
                            (left.meta & mask) as u32,
                            (right.meta >> params.k()) as u32,
                            (right.meta & mask) as u32,
                        ],
                    });
                }
            }
        }
    }
    output.sort_unstable_by_key(|quad| (quad.info, quad.meta, quad.xs));
    Ok(output)
}
