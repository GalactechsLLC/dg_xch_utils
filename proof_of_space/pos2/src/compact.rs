use crate::compute::{
    BATCH_SIZE, CpuHasher, HashEngine, SCRATCH_BYTES, Work, allocate, check_cancelled, config,
};
use crate::device::{self, Record};
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

#[path = "compact_cpu.rs"]
mod cpu;
#[cfg(feature = "vulkan")]
#[path = "compact_vulkan.rs"]
pub(crate) mod gpu;
#[cfg(feature = "resident")]
#[path = "compact_resident.rs"]
pub mod resident;

#[repr(C)]
#[derive(Clone, Copy, Default)]
#[cfg_attr(feature = "resident", derive(bytemuck::Pod, bytemuck::Zeroable))]
pub struct Entry {
    pub meta: u64,
    pub info: u32,
    pub x_bits: u32,
}

impl Entry {
    fn record(self) -> Record {
        Record {
            meta: self.meta,
            info: self.info,
            x_bits: self.x_bits,
            ..Record::default()
        }
    }
}

pub struct CompactPlot {
    params: ProofParams,
    fragments: Vec<u64>,
    pub table_counts: [usize; 4],
}

pub struct PackedChunk {
    pub count: u32,
    pub deltas: Vec<u8>,
    pub stubs: Vec<u8>,
}

impl CompactPlot {
    pub fn params(&self) -> &ProofParams {
        &self.params
    }
    pub fn fragments(&self) -> &[u64] {
        &self.fragments
    }

    pub fn from_sorted_fragments(
        params: ProofParams,
        fragments: Vec<u64>,
        table_counts: [usize; 4],
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancelled)?;
        if table_counts[0] as u64 != 1u64 << params.k() || table_counts[3] != fragments.len() {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "compact plot table counts do not match its fragments",
            ));
        }
        let maximum = u64::MAX >> (64 - u32::from(params.k()) * 2);
        let mut previous = 0;
        for chunk in fragments.chunks(4096) {
            check_cancelled(cancelled)?;
            for &fragment in chunk {
                if fragment < previous || fragment > maximum {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "compact plot fragments are unsorted or exceed the plot size",
                    ));
                }
                previous = fragment;
            }
        }
        check_cancelled(cancelled)?;
        Ok(Self {
            params,
            fragments,
            table_counts,
        })
    }

    pub fn minimum_work(params: &ProofParams) -> u64 {
        (1u64 << params.k()) * ((1u64 << params.strength()) + 1)
    }

    pub fn memory_required(k: u8, max_entries: usize) -> Result<u64, Error> {
        if !(18..=32).contains(&k) || !k.is_multiple_of(2) {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid plot size"));
        }
        let initial = 1u64
            .checked_shl(u32::from(k))
            .ok_or_else(|| Error::other("invalid plot size"))?;
        let capacity = (initial + initial / 8 + 65_536).min(max_entries as u64);
        if capacity < initial {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "entry budget is smaller than table one",
            ));
        }
        capacity
            .checked_mul(2 * size_of::<Entry>() as u64)
            .and_then(|size| size.checked_add(SCRATCH_BYTES))
            .ok_or_else(|| Error::other("plot memory size overflow"))
    }

    pub fn build(
        params: ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        if params.k() == 28 && params.strength() == 2 {
            return cpu::build(params, limits, cancelled);
        }
        let mut engine = CpuHasher::new(&params);
        Self::build_with_engine(params, limits, cancelled, &mut engine)
    }

    pub fn build_with_engine(
        params: ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
        engine: &mut impl HashEngine,
    ) -> Result<Self, Error> {
        check_cancelled(cancelled)?;
        if let Some(plot) = engine.build_compact(&params, limits, cancelled)? {
            if plot.params() != &params {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "compact plot engine returned different parameters",
                ));
            }
            check_cancelled(cancelled)?;
            return Ok(plot);
        }
        if params.k() == 28 && params.strength() == 2 && engine.is_accelerated() {
            return cpu::build_with_engine(params, limits, cancelled, engine);
        }
        if limits.max_work < Self::minimum_work(&params) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "work budget is below the {} evaluations required just for generation and table-one targets",
                    Self::minimum_work(&params)
                ),
            ));
        }
        let required = Self::memory_required(params.k(), limits.max_entries)?;
        if required > limits.memory_bytes {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("compact RAM plotting needs at least {required} managed bytes"),
            ));
        }
        let capacity =
            usize::try_from((required - SCRATCH_BYTES) / (2 * size_of::<Entry>() as u64))
                .map_err(|_| Error::other("plot exceeds platform address space"))?;
        let initial = 1u64 << params.k();
        let configuration = config(&params);
        let mut work = Work::new(limits, cancelled);
        let mut entries = allocate::<Entry>(capacity)?;
        let mut output = allocate::<Entry>(capacity)?;
        let mut inputs = allocate(BATCH_SIZE)?;
        let profile = std::env::var_os("DGX_POS2_PROFILE").is_some();
        let mut started = Instant::now();
        for start in (0..initial).step_by(BATCH_SIZE) {
            inputs.clear();
            for value in start..(start + BATCH_SIZE as u64).min(initial) {
                inputs.push([
                    value as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                    0,
                    0,
                    0,
                ]);
            }
            for (position, lanes) in work.hash(engine, &inputs, 16)?.into_iter().enumerate() {
                let generated = device::generate_from_hash(
                    configuration,
                    (start + position as u64) as u32,
                    lanes,
                );
                entries.push(Entry {
                    meta: generated.meta,
                    info: generated.info,
                    x_bits: 0,
                });
            }
        }
        if profile {
            eprintln!(
                "plot_phase phase=generation seconds={:.6}",
                started.elapsed().as_secs_f64()
            );
        }
        started = Instant::now();
        crate::radix::sort(
            &mut entries,
            &mut output,
            u32::from(params.k()),
            |entry| u64::from(entry.info),
            cancelled,
        )?;
        output.clear();
        if profile {
            eprintln!(
                "plot_phase phase=sort table=0 seconds={:.6}",
                started.elapsed().as_secs_f64()
            );
        }
        let mut counts = [entries.len(), 0, 0, 0];
        for table in 1..=3u32 {
            check_cancelled(cancelled)?;
            output.clear();
            let mut pairs = allocate::<(Entry, Entry)>(BATCH_SIZE)?;
            let keys = params.num_match_keys(table as usize);
            let targets = (entries.len() as u64)
                .checked_mul(keys)
                .ok_or_else(|| Error::other("target count overflow"))?;
            let rounds = if table == 1 {
                16 << (u32::from(params.strength()) - 2)
            } else {
                16
            };
            let spare_entries = (limits.memory_bytes - required) / size_of::<usize>() as u64;
            let bucket_bits = if spare_entries > 65_537 {
                (63 - (spare_entries - 1).leading_zeros()).min(u32::from(params.k()))
            } else {
                u32::from(params.k().min(16))
            };
            let shift = u32::from(params.k()) - bucket_bits;
            let mut buckets = allocate::<usize>((1usize << bucket_bits) + 1)?;
            let mut position = 0;
            started = Instant::now();
            for bucket in 0..=1u64 << bucket_bits {
                if bucket.is_multiple_of(BATCH_SIZE as u64) {
                    check_cancelled(cancelled)?;
                }
                while position < entries.len()
                    && u64::from(entries[position].info >> shift) < bucket
                {
                    position += 1;
                }
                buckets.push(position);
            }
            if profile {
                eprintln!(
                    "plot_phase phase=index table={table} seconds={:.6}",
                    started.elapsed().as_secs_f64()
                );
            }
            started = Instant::now();
            let flush = |pairs: &mut Vec<(Entry, Entry)>,
                         output: &mut Vec<Entry>,
                         work: &mut Work<'_>,
                         engine: &mut _|
             -> Result<(), Error> {
                if pairs.is_empty() {
                    return Ok(());
                }
                let mut pair_inputs = allocate(pairs.len())?;
                for (left, right) in pairs.iter() {
                    pair_inputs.push([
                        left.meta as u32,
                        (left.meta >> 32) as u32,
                        right.meta as u32,
                        (right.meta >> 32) as u32,
                    ]);
                }
                let hashes = work.hash(engine, &pair_inputs, rounds)?;
                let mut results = allocate(pairs.len())?;
                results.resize(pairs.len(), None);
                results
                    .par_iter_mut()
                    .zip(pairs.par_iter())
                    .zip(hashes.par_iter())
                    .for_each(|((slot, (left, right)), lanes)| {
                        let result = device::pair_from_hash(
                            configuration,
                            table,
                            left.record(),
                            right.record(),
                            *lanes,
                        );
                        if result.valid != 0 {
                            *slot = Some(Entry {
                                meta: if table == 3 {
                                    result.fragment
                                } else {
                                    result.meta
                                },
                                info: result.info,
                                x_bits: result.x_bits,
                            });
                        }
                    });
                pairs.clear();
                for result in results.into_iter().flatten() {
                    if output.len() == capacity {
                        return Err(Error::other("compact table exceeds entry budget"));
                    }
                    output.push(result);
                }
                Ok(())
            };
            for start in (0..targets).step_by(BATCH_SIZE) {
                inputs.clear();
                let end = (start + BATCH_SIZE as u64).min(targets);
                for position in start..end {
                    let left = entries[(position / keys) as usize];
                    inputs.push([
                        table,
                        (position % keys) as u32,
                        left.meta as u32,
                        (left.meta >> 32) as u32,
                    ]);
                }
                let hashes = work.hash(engine, &inputs, rounds)?;
                let mut matches = allocate(hashes.len())?;
                matches.resize(hashes.len(), (0usize, 0usize));
                matches
                    .par_iter_mut()
                    .zip(hashes.par_iter())
                    .enumerate()
                    .for_each(|(offset, (matched, lanes))| {
                        let position = start + offset as u64;
                        let left = entries[(position / keys) as usize];
                        let target = device::target_from_hash(
                            configuration,
                            table,
                            left.record(),
                            (position % keys) as u32,
                            lanes[0],
                        );
                        let bucket = (target >> shift) as usize;
                        if shift == 0 {
                            *matched = (buckets[bucket], buckets[bucket + 1] - buckets[bucket]);
                            return;
                        }
                        let candidates = &entries[buckets[bucket]..buckets[bucket + 1]];
                        let first = candidates.partition_point(|entry| entry.info < target);
                        let count =
                            candidates[first..].partition_point(|entry| entry.info == target);
                        *matched = (buckets[bucket] + first, count);
                    });
                for (position, (first, count)) in (start..end).zip(matches) {
                    let left = entries[(position / keys) as usize];
                    for right in &entries[first..first + count] {
                        pairs.push((left, *right));
                        if pairs.len() == BATCH_SIZE {
                            flush(&mut pairs, &mut output, &mut work, engine)?;
                        }
                    }
                }
            }
            flush(&mut pairs, &mut output, &mut work, engine)?;
            drop(buckets);
            if profile {
                eprintln!(
                    "plot_phase phase=matching table={table} seconds={:.6}",
                    started.elapsed().as_secs_f64()
                );
            }
            started = Instant::now();
            entries.clear();
            check_cancelled(cancelled)?;
            if table == 3 {
                crate::radix::sort(
                    &mut output,
                    &mut entries,
                    2 * u32::from(params.k()),
                    |entry| entry.meta,
                    cancelled,
                )?;
            } else {
                crate::radix::sort(
                    &mut output,
                    &mut entries,
                    u32::from(params.k()),
                    |entry| u64::from(entry.info),
                    cancelled,
                )?;
            }
            if profile {
                eprintln!(
                    "plot_phase phase=sort table={table} seconds={:.6}",
                    started.elapsed().as_secs_f64()
                );
            }
            counts[table as usize] = output.len();
            std::mem::swap(&mut entries, &mut output);
        }
        drop(output);
        let mut fragments = allocate(entries.len())?;
        fragments.extend(entries.iter().map(|entry| entry.meta));
        check_cancelled(cancelled)?;
        Ok(Self {
            params,
            fragments,
            table_counts: counts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct SpecializedEngine {
        builds: usize,
        hashes: usize,
        fail: bool,
        different_params: bool,
    }

    impl HashEngine for SpecializedEngine {
        fn is_accelerated(&self) -> bool {
            true
        }

        fn build_compact(
            &mut self,
            params: &ProofParams,
            _limits: PlotLimits,
            _cancelled: &AtomicBool,
        ) -> Result<Option<CompactPlot>, Error> {
            self.builds += 1;
            if self.fail {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "test specialized failure",
                ));
            }
            Ok(Some(CompactPlot {
                params: if self.different_params {
                    ProofParams::new([31; 32].into(), 28, 2, false)?
                } else {
                    params.clone()
                },
                fragments: Vec::new(),
                table_counts: [0; 4],
            }))
        }

        fn hash(
            &mut self,
            _inputs: &[[u32; 4]],
            _rounds: u32,
            _cancelled: &AtomicBool,
        ) -> Result<Vec<[u32; 4]>, Error> {
            self.hashes += 1;
            Err(Error::other("unexpected generic hash execution"))
        }
    }

    fn params() -> ProofParams {
        ProofParams::new([17; 32].into(), 28, 2, false).unwrap()
    }

    #[test]
    fn sorted_fragment_constructor_validates_counts_order_and_size() {
        let cancelled = AtomicBool::new(false);
        let counts = [1 << 28, 10, 9, 4];
        let fragments = vec![0, 1, 1, (1 << 56) - 1];
        let plot =
            CompactPlot::from_sorted_fragments(params(), fragments.clone(), counts, &cancelled)
                .unwrap();
        assert_eq!(plot.fragments(), fragments);
        assert_eq!(plot.table_counts, counts);
        for (fragments, counts) in [
            (vec![0, 2, 1, 3], counts),
            (vec![0, 1, 2, 1 << 56], counts),
            (vec![0], counts),
            (vec![0, 1, 2, 3], [0, 10, 9, 4]),
        ] {
            assert!(
                CompactPlot::from_sorted_fragments(params(), fragments, counts, &cancelled)
                    .is_err()
            );
        }
        let error = CompactPlot::from_sorted_fragments(
            params(),
            Vec::new(),
            [1 << 28, 0, 0, 0],
            &AtomicBool::new(true),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        if let Ok(initial_entries) = usize::try_from(1u64 << 32) {
            let params = ProofParams::new([17; 32].into(), 32, 2, false).unwrap();
            assert!(
                CompactPlot::from_sorted_fragments(
                    params,
                    vec![u64::MAX],
                    [initial_entries, 1, 1, 1],
                    &cancelled,
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn specialized_k28_builder_precedes_generic_pipeline() {
        let params = params();
        let cancelled = AtomicBool::new(false);
        let mut engine = SpecializedEngine::default();
        let plot = CompactPlot::build_with_engine(
            params.clone(),
            PlotLimits::default(),
            &cancelled,
            &mut engine,
        )
        .unwrap();
        assert_eq!(plot.params(), &params);
        assert_eq!(engine.builds, 1);
        assert_eq!(engine.hashes, 0);

        let mut boxed = Box::<SpecializedEngine>::default();
        let plot = CompactPlot::build_with_engine(
            params.clone(),
            PlotLimits::default(),
            &cancelled,
            &mut boxed,
        )
        .unwrap();
        assert_eq!(plot.params(), &params);
        assert_eq!(boxed.builds, 1);
        assert_eq!(boxed.hashes, 0);
    }

    #[test]
    fn specialized_k28_errors_do_not_fall_back() {
        let mut engine = SpecializedEngine {
            fail: true,
            ..SpecializedEngine::default()
        };
        let error = CompactPlot::build_with_engine(
            params(),
            PlotLimits::default(),
            &AtomicBool::new(false),
            &mut engine,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert_eq!(engine.builds, 1);
        assert_eq!(engine.hashes, 0);
    }

    #[test]
    fn specialized_k28_parameters_are_checked() {
        let mut engine = SpecializedEngine {
            different_params: true,
            ..SpecializedEngine::default()
        };
        let error = CompactPlot::build_with_engine(
            params(),
            PlotLimits::default(),
            &AtomicBool::new(false),
            &mut engine,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(engine.builds, 1);
        assert_eq!(engine.hashes, 0);
    }

    #[test]
    fn cancelled_k28_builder_does_not_invoke_specialized_engine() {
        let mut engine = SpecializedEngine::default();
        let error = CompactPlot::build_with_engine(
            params(),
            PlotLimits::default(),
            &AtomicBool::new(true),
            &mut engine,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        assert_eq!(engine.builds, 0);
        assert_eq!(engine.hashes, 0);
    }
}
