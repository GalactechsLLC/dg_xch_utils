use super::bits::compact_bits;
use super::chainer::{Chain, Chainer, SearchLimits};
use super::core::ProofCore;
use super::params::ProofParams;
use super::validator::ProofValidator;
use dg_xch_core::blockchain::sized_bytes::Bytes32;
use std::io::{Error, ErrorKind};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug)]
pub struct PlotLimits {
    pub memory_bytes: u64,
    pub max_entries: usize,
    pub max_work: u64,
}

impl Default for PlotLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 512 * 1024 * 1024,
            max_entries: 2_097_152,
            max_work: 100_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    meta: u64,
    match_info: u32,
    x_bits: u32,
    xs: [u32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Witness {
    pub fragment: u64,
    pub xs: [u32; 8],
}

pub struct NativePlot {
    params: ProofParams,
    witnesses: Vec<Witness>,
    pub table_counts: [usize; 4],
}

struct Budget<'a> {
    remaining: u64,
    cancelled: &'a AtomicBool,
}

impl Budget<'_> {
    fn step(&mut self) -> Result<(), Error> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(Error::new(
                ErrorKind::Interrupted,
                "native plotting cancelled",
            ));
        }
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or_else(|| Error::other("native plotting work limit exceeded"))?;
        Ok(())
    }
}

fn allocate<T>(capacity: usize) -> Result<Vec<T>, Error> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(capacity)
        .map_err(|_| Error::other("native plotting allocation failed"))?;
    Ok(entries)
}

fn push<T>(entries: &mut Vec<T>, value: T, limit: usize) -> Result<(), Error> {
    if entries.len() == limit {
        return Err(Error::other("native plotting table entry limit exceeded"));
    }
    entries.push(value);
    Ok(())
}

fn matches(
    core: &ProofCore,
    table: usize,
    entries: &[Entry],
    left: &Entry,
    key: u32,
) -> std::ops::Range<usize> {
    let params = core.params();
    let section = core.matching_section(params.extract_section(left.match_info));
    let target = core.matching_target(table, left.meta, key);
    let info = (section << (u32::from(params.k()) - params.num_section_bits()))
        | (key << params.num_match_target_bits(table))
        | target;
    let start = entries.partition_point(|entry| entry.match_info < info);
    let end = entries.partition_point(|entry| entry.match_info <= info);
    start..end
}

impl NativePlot {
    pub fn from_witnesses(
        params: ProofParams,
        witnesses: Vec<Witness>,
        table_counts: [usize; 4],
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        if witnesses.len() != table_counts[3]
            || witnesses
                .windows(2)
                .any(|pair| (pair[0].fragment, pair[0].xs) > (pair[1].fragment, pair[1].xs))
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid accelerator output ordering or count",
            ));
        }
        let validator = ProofValidator::new(params.clone())?;
        for witness in &witnesses {
            if cancelled.load(Ordering::Relaxed) {
                return Err(Error::new(
                    ErrorKind::Interrupted,
                    "accelerator validation cancelled",
                ));
            }
            if validator.validate_table_3_pairs(&witness.xs).is_none()
                || validator.core().fragment_codec.encode(&witness.xs) != witness.fragment
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "accelerator witness failed independent CPU verification",
                ));
            }
        }
        Ok(Self {
            params,
            witnesses,
            table_counts,
        })
    }

    pub fn build(
        params: ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        let count = 1u64 << params.k();
        let per_entry = (2 * size_of::<Entry>() + size_of::<Witness>()) as u64;
        let capacity = limits.max_entries.min(
            (limits.memory_bytes / per_entry)
                .min(count * 2)
                .min(usize::MAX as u64) as usize,
        );
        if count > capacity as u64 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "native in-memory plot exceeds memory/entry budget; increase limits or use a smaller development k",
            ));
        }
        let core = ProofCore::new(params.clone())?;
        let mut budget = Budget {
            remaining: limits.max_work,
            cancelled,
        };
        budget.step()?;
        let mut entries = allocate(capacity)?;
        for value in 0..count {
            budget.step()?;
            let value = value as u32;
            entries.push(Entry {
                meta: u64::from(value),
                match_info: core.hashing.g(value),
                x_bits: 0,
                xs: [value, 0, 0, 0],
            });
        }
        entries.sort_unstable_by_key(|entry| (entry.match_info, entry.meta, entry.xs));
        let mut table_counts = [entries.len(), 0, 0, 0];
        for table in 1..=2 {
            let mut output = allocate(capacity)?;
            for left in &entries {
                for key in 0..params.num_match_keys(table) {
                    budget.step()?;
                    for right in &entries[matches(&core, table, &entries, left, key as u32)] {
                        budget.step()?;
                        let entry = if table == 1 {
                            core.pairing_t1(left.xs[0], right.xs[0]).map(|pair| Entry {
                                meta: pair.meta,
                                match_info: pair.match_info,
                                x_bits: 0,
                                xs: [left.xs[0], right.xs[0], 0, 0],
                            })
                        } else {
                            core.pairing_t2(left.meta, right.meta).map(|pair| Entry {
                                meta: pair.meta,
                                match_info: pair.match_info,
                                x_bits: pair.x_bits,
                                xs: [left.xs[0], left.xs[1], right.xs[0], right.xs[1]],
                            })
                        };
                        if let Some(entry) = entry {
                            push(&mut output, entry, capacity)?;
                        }
                    }
                }
            }
            output.sort_unstable_by_key(|entry| (entry.match_info, entry.meta, entry.xs));
            table_counts[table] = output.len();
            entries = output;
        }
        let mut witnesses = allocate(capacity)?;
        for left in &entries {
            for key in 0..params.num_match_keys(3) {
                budget.step()?;
                for right in &entries[matches(&core, 3, &entries, left, key as u32)] {
                    budget.step()?;
                    if let Some(pair) =
                        core.pairing_t3(left.meta, right.meta, left.x_bits, right.x_bits)
                    {
                        let mut xs = [0; 8];
                        xs[..4].copy_from_slice(&left.xs);
                        xs[4..].copy_from_slice(&right.xs);
                        push(
                            &mut witnesses,
                            Witness {
                                fragment: pair.proof_fragment,
                                xs,
                            },
                            capacity,
                        )?;
                    }
                }
            }
        }
        budget.step()?;
        witnesses.sort_unstable_by_key(|witness| (witness.fragment, witness.xs));
        budget.step()?;
        table_counts[3] = witnesses.len();
        Ok(Self {
            params,
            witnesses,
            table_counts,
        })
    }

    pub fn witnesses(&self) -> &[Witness] {
        &self.witnesses
    }

    pub fn params(&self) -> &ProofParams {
        &self.params
    }

    pub fn qualities(
        &self,
        challenge: Bytes32,
        limits: SearchLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<Chain>, Error> {
        let core = ProofCore::new(self.params.clone())?;
        let ranges = core.select_challenge_sets(challenge).ranges;
        let mut sets: [Vec<u64>; 4] = std::array::from_fn(|_| Vec::new());
        for (index, range) in ranges.iter().enumerate() {
            let start = self
                .witnesses
                .partition_point(|witness| witness.fragment < range.start);
            let end = self
                .witnesses
                .partition_point(|witness| witness.fragment <= range.end);
            if end - start > 4096 {
                return Err(Error::other("challenge set exceeds native search limit"));
            }
            sets[index] = self.witnesses[start..end]
                .iter()
                .map(|witness| witness.fragment)
                .collect();
            sets[index].dedup();
        }
        Chainer::new(&core, challenge)
            .search(
                std::array::from_fn(|index| sets[index].as_slice()),
                limits,
                cancelled,
            )
            .map_err(|error| Error::other(format!("native chain search: {error:?}")))
    }

    pub fn prove(&self, chain: &Chain, challenge: Bytes32) -> Result<Vec<u8>, Error> {
        let mut values = [0; 128];
        for (index, fragment) in chain.fragments.iter().enumerate() {
            let position = self
                .witnesses
                .partition_point(|witness| witness.fragment < *fragment);
            let witness = self
                .witnesses
                .get(position)
                .filter(|witness| witness.fragment == *fragment)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "fragment absent from plot"))?;
            values[index * 8..index * 8 + 8].copy_from_slice(&witness.xs);
        }
        let validator = ProofValidator::new(self.params.clone())?;
        if validator.validate_full_proof(&values, challenge) != Some(chain.fragments) {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "native plot witness failed verification",
            ));
        }
        Ok(compact_bits(&values, self.params.k()))
    }
}
