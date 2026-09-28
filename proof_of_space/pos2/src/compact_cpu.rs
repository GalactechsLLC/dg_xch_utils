use super::{CompactPlot, Entry};
use crate::compute::{
    CpuHasher, GPU_BATCH_SIZE, HashEngine, SCRATCH_BYTES, allocate, check_cancelled, config,
};
use crate::device::{self, Config};
use crate::params::ProofParams;
use crate::plotting::PlotLimits;
use crate::radix;
use rayon::prelude::*;
use std::io::{Error, ErrorKind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

const BATCH: usize = 4096;
const MAX_WORKERS: usize = 32;
const MAX_GPU_WORKERS: usize = 8;
const GPU_SCRATCH_BYTES: u64 = 320 * 1024 * 1024;
const INDEX_BATCH: usize = 65_536;

enum Backend<'engine> {
    Cpu(CpuHasher),
    Accelerator(Mutex<&'engine mut dyn HashEngine>),
}

impl Backend<'_> {
    fn hash(
        &self,
        inputs: &[[u32; 4]],
        rounds: u32,
        output: &mut [[u32; 4]],
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        match self {
            Self::Cpu(hasher) => hasher.hash_into_serial(inputs, rounds, output, cancelled),
            Self::Accelerator(engine) => {
                check_cancelled(cancelled)?;
                let mut engine = engine
                    .lock()
                    .map_err(|_| Error::other("PoS2 accelerator lock poisoned"))?;
                check_cancelled(cancelled)?;
                let hashes = engine.hash(inputs, rounds, cancelled)?;
                if hashes.len() != output.len() {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "hash engine returned wrong batch length",
                    ));
                }
                output.copy_from_slice(&hashes);
                Ok(())
            }
        }
    }
}

struct Context<'engine, 'cancel> {
    configuration: Config,
    backend: Backend<'engine>,
    remaining: AtomicU64,
    cancelled: &'cancel AtomicBool,
    capacity: usize,
    batch_size: usize,
}

impl Context<'_, '_> {
    fn charge(&self, amount: u64) -> Result<(), Error> {
        check_cancelled(self.cancelled)?;
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(amount)
            })
            .map(|_| ())
            .map_err(|_| Error::other("PoS2 work budget exceeded"))
    }
}

struct HashBatch {
    inputs: Vec<[u32; 4]>,
    output: Vec<[u32; 4]>,
    scratch: Vec<[u32; 4]>,
}

impl HashBatch {
    fn new(capacity: usize) -> Result<Self, Error> {
        let mut output = allocate(capacity)?;
        output.resize(capacity, [0; 4]);
        let mut scratch = allocate(capacity)?;
        scratch.resize(capacity, [0; 4]);
        Ok(Self {
            inputs: allocate(capacity)?,
            output,
            scratch,
        })
    }

    fn hash(&mut self, context: &Context<'_, '_>, rounds: u32) -> Result<(), Error> {
        let count = self.inputs.len();
        let first_rounds = rounds.min(1024);
        context.backend.hash(
            &self.inputs,
            first_rounds,
            &mut self.output[..count],
            context.cancelled,
        )?;
        let mut remaining = rounds - first_rounds;
        while remaining > 0 {
            let next_rounds = remaining.min(1024);
            context.backend.hash(
                &self.output[..count],
                next_rounds,
                &mut self.scratch[..count],
                context.cancelled,
            )?;
            std::mem::swap(&mut self.output, &mut self.scratch);
            remaining -= next_rounds;
        }
        Ok(())
    }
}

struct PairBatch {
    pairs: Vec<(Entry, Entry)>,
    hashes: HashBatch,
    valid: Vec<Entry>,
}

impl PairBatch {
    fn new(capacity: usize) -> Result<Self, Error> {
        Ok(Self {
            pairs: allocate(capacity)?,
            hashes: HashBatch::new(capacity)?,
            valid: allocate(capacity)?,
        })
    }
}

struct Table<'context, 'engine, 'cancel> {
    context: &'context Context<'engine, 'cancel>,
    entries: &'context [Entry],
    buckets: &'context [usize],
    output: &'context Mutex<Vec<Entry>>,
    shift: u32,
    number: u32,
    rounds: u32,
    keys: u64,
}

impl Table<'_, '_, '_> {
    fn flush_valid(&self, valid: &mut Vec<Entry>) -> Result<(), Error> {
        if valid.is_empty() {
            return Ok(());
        }
        let mut output = self
            .output
            .lock()
            .map_err(|_| Error::other("PoS2 table output lock poisoned"))?;
        if valid.len() > self.context.capacity.saturating_sub(output.len()) {
            return Err(Error::other("compact table exceeds entry budget"));
        }
        output.extend_from_slice(valid);
        valid.clear();
        Ok(())
    }

    fn flush_pairs(&self, batch: &mut PairBatch) -> Result<(), Error> {
        if batch.pairs.is_empty() {
            return Ok(());
        }
        self.context.charge(
            (batch.pairs.len() as u64)
                .checked_mul(u64::from(self.rounds / 16))
                .ok_or_else(|| Error::other("PoS2 pair work overflow"))?,
        )?;
        batch.hashes.inputs.clear();
        for (left, right) in &batch.pairs {
            batch.hashes.inputs.push([
                left.meta as u32,
                (left.meta >> 32) as u32,
                right.meta as u32,
                (right.meta >> 32) as u32,
            ]);
        }
        batch.hashes.hash(self.context, self.rounds)?;
        for ((left, right), lanes) in batch.pairs.iter().zip(&batch.hashes.output) {
            let result = device::pair_from_hash(
                self.context.configuration,
                self.number,
                left.record(),
                right.record(),
                *lanes,
            );
            if result.valid != 0 {
                batch.valid.push(Entry {
                    meta: if self.number == 3 {
                        result.fragment
                    } else {
                        result.meta
                    },
                    info: result.info,
                    x_bits: result.x_bits,
                });
                if batch.valid.len() == self.context.batch_size {
                    self.flush_valid(&mut batch.valid)?;
                }
            }
        }
        batch.pairs.clear();
        Ok(())
    }

    fn process(&self, left_entries: &[Entry]) -> Result<(), Error> {
        let mut targets = HashBatch::new(self.context.batch_size)?;
        let mut pairs = PairBatch::new(self.context.batch_size)?;
        let count = (left_entries.len() as u64)
            .checked_mul(self.keys)
            .ok_or_else(|| Error::other("PoS2 target count overflow"))?;
        for start in (0..count).step_by(self.context.batch_size) {
            check_cancelled(self.context.cancelled)?;
            let end = (start + self.context.batch_size as u64).min(count);
            targets.inputs.clear();
            for position in start..end {
                let left = left_entries[(position / self.keys) as usize];
                targets.inputs.push([
                    self.number,
                    (position % self.keys) as u32,
                    left.meta as u32,
                    (left.meta >> 32) as u32,
                ]);
            }
            targets.hash(self.context, self.rounds)?;
            for (position, lanes) in (start..end).zip(&targets.output) {
                let left = left_entries[(position / self.keys) as usize];
                let target = device::target_from_hash(
                    self.context.configuration,
                    self.number,
                    left.record(),
                    (position % self.keys) as u32,
                    lanes[0],
                );
                let bucket = (target >> self.shift) as usize;
                let mut candidates = &self.entries[self.buckets[bucket]..self.buckets[bucket + 1]];
                if self.shift != 0 {
                    let first = candidates.partition_point(|entry| entry.info < target);
                    candidates = &candidates[first..];
                    let matched = candidates.partition_point(|entry| entry.info == target);
                    candidates = &candidates[..matched];
                }
                for right in candidates {
                    pairs.pairs.push((left, *right));
                    if pairs.pairs.len() == self.context.batch_size {
                        self.flush_pairs(&mut pairs)?;
                    }
                }
            }
        }
        self.flush_pairs(&mut pairs)?;
        self.flush_valid(&mut pairs.valid)
    }
}

fn build_index(
    entries: &[Entry],
    bits: u32,
    shift: u32,
    cancelled: &AtomicBool,
) -> Result<Vec<usize>, Error> {
    let length = usize::try_from((1u64 << bits) + 1)
        .map_err(|_| Error::other("PoS2 index exceeds platform address space"))?;
    let mut buckets = allocate(length)?;
    while buckets.len() < length {
        check_cancelled(cancelled)?;
        buckets.resize((buckets.len() + INDEX_BATCH).min(length), 0usize);
    }
    buckets
        .par_chunks_mut(INDEX_BATCH)
        .enumerate()
        .try_for_each(|(chunk, buckets)| -> Result<(), Error> {
            let first_bucket = chunk * INDEX_BATCH;
            let first_info = (first_bucket as u64) << shift;
            let mut position = entries.partition_point(|entry| u64::from(entry.info) < first_info);
            for (offset, bucket) in buckets.iter_mut().enumerate() {
                if offset.is_multiple_of(BATCH) {
                    check_cancelled(cancelled)?;
                }
                let info = (first_bucket + offset) as u64;
                while position < entries.len() && u64::from(entries[position].info >> shift) < info
                {
                    position += 1;
                }
                *bucket = position;
            }
            Ok(())
        })?;
    Ok(buckets)
}

pub(super) fn build(
    params: ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
) -> Result<CompactPlot, Error> {
    let backend = Backend::Cpu(CpuHasher::new(&params));
    build_inner(params, limits, cancelled, backend)
}

pub(super) fn build_with_engine(
    params: ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    engine: &mut impl HashEngine,
) -> Result<CompactPlot, Error> {
    build_inner(
        params,
        limits,
        cancelled,
        Backend::Accelerator(Mutex::new(engine)),
    )
}

fn build_inner(
    params: ProofParams,
    limits: PlotLimits,
    cancelled: &AtomicBool,
    backend: Backend<'_>,
) -> Result<CompactPlot, Error> {
    check_cancelled(cancelled)?;
    if limits.max_work < CompactPlot::minimum_work(&params) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "work budget is below generation and table-one target requirements",
        ));
    }
    let accelerated = matches!(&backend, Backend::Accelerator(_));
    let base_required = CompactPlot::memory_required(params.k(), limits.max_entries)?;
    let required = base_required
        .checked_add(if accelerated { GPU_SCRATCH_BYTES } else { 0 })
        .ok_or_else(|| Error::other("compact RAM plotting memory size overflow"))?;
    if required > limits.memory_bytes {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("compact RAM plotting needs at least {required} managed bytes"),
        ));
    }
    let capacity =
        usize::try_from((base_required - SCRATCH_BYTES) / (2 * size_of::<Entry>() as u64))
            .map_err(|_| Error::other("plot exceeds platform address space"))?;
    let initial = usize::try_from(1u64 << params.k())
        .map_err(|_| Error::other("plot exceeds platform address space"))?;
    let context = Context {
        configuration: config(&params),
        backend,
        remaining: AtomicU64::new(limits.max_work),
        cancelled,
        capacity,
        batch_size: if accelerated { GPU_BATCH_SIZE } else { BATCH },
    };
    context.charge(initial as u64)?;
    let workers = rayon::current_num_threads().clamp(
        1,
        if accelerated {
            MAX_GPU_WORKERS
        } else {
            MAX_WORKERS
        },
    );
    let profile = std::env::var_os("DGX_POS2_PROFILE").is_some();
    let profile_name = if accelerated {
        "pos2_hybrid"
    } else {
        "pos2_cpu"
    };
    let started = Instant::now();
    let mut entries = allocate::<Entry>(capacity)?;
    while entries.len() < initial {
        check_cancelled(cancelled)?;
        entries.resize((entries.len() + BATCH).min(initial), Entry::default());
    }
    let mut scratch = allocate::<Entry>(capacity)?;
    let chunk_size = initial.div_ceil(workers).max(1);
    entries
        .par_chunks_mut(chunk_size)
        .enumerate()
        .try_for_each(|(chunk, entries)| -> Result<(), Error> {
            let mut hashes = HashBatch::new(context.batch_size)?;
            let start = chunk * chunk_size;
            for (batch, entries) in entries.chunks_mut(context.batch_size).enumerate() {
                let first = start + batch * context.batch_size;
                hashes.inputs.clear();
                for offset in 0..entries.len() {
                    hashes.inputs.push([
                        (first + offset) as u32 ^ if params.is_testnet() { 0xA3B1C4D7 } else { 0 },
                        0,
                        0,
                        0,
                    ]);
                }
                hashes.hash(&context, 16)?;
                for (offset, (entry, lanes)) in entries.iter_mut().zip(&hashes.output).enumerate() {
                    let generated = device::generate_from_hash(
                        context.configuration,
                        (first + offset) as u32,
                        *lanes,
                    );
                    *entry = Entry {
                        meta: generated.meta,
                        info: generated.info,
                        x_bits: 0,
                    };
                }
            }
            Ok(())
        })?;
    if profile {
        eprintln!(
            "{profile_name} generation seconds={:.3}",
            started.elapsed().as_secs_f64()
        );
    }
    let started = Instant::now();
    radix::sort(
        &mut entries,
        &mut scratch,
        u32::from(params.k()),
        |entry| u64::from(entry.info),
        cancelled,
    )?;
    if profile {
        eprintln!(
            "{profile_name} sort_0 seconds={:.3}",
            started.elapsed().as_secs_f64()
        );
    }
    let mut counts = [entries.len(), 0, 0, 0];
    let spare_entries = (limits.memory_bytes - required) / size_of::<usize>() as u64;
    let bucket_bits = if spare_entries > 65_537 {
        (63 - (spare_entries - 1).leading_zeros()).min(u32::from(params.k()))
    } else {
        u32::from(params.k().min(16))
    };
    let shift = u32::from(params.k()) - bucket_bits;
    for number in 1..=3u32 {
        let keys = params.num_match_keys(number as usize);
        let rounds = if number == 1 {
            16 << (u32::from(params.strength()) - 2)
        } else {
            16
        };
        context.charge(
            (entries.len() as u64)
                .checked_mul(keys)
                .and_then(|targets| targets.checked_mul(u64::from(rounds / 16)))
                .ok_or_else(|| Error::other("PoS2 target work overflow"))?,
        )?;
        let started = Instant::now();
        let buckets = build_index(&entries, bucket_bits, shift, cancelled)?;
        if profile {
            eprintln!(
                "{profile_name} index_{number} seconds={:.3}",
                started.elapsed().as_secs_f64()
            );
        }
        scratch.clear();
        let output = Mutex::new(std::mem::take(&mut scratch));
        let table = Table {
            context: &context,
            entries: &entries,
            buckets: &buckets,
            output: &output,
            shift,
            number,
            rounds,
            keys,
        };
        let started = Instant::now();
        entries
            .par_chunks(entries.len().div_ceil(workers).max(1))
            .try_for_each(|entries| table.process(entries))?;
        scratch = output
            .into_inner()
            .map_err(|_| Error::other("PoS2 table output lock poisoned"))?;
        if profile {
            eprintln!(
                "{profile_name} matching_{number} seconds={:.3} entries={}",
                started.elapsed().as_secs_f64(),
                scratch.len()
            );
        }
        drop(buckets);
        let started = Instant::now();
        let bits = if number == 3 {
            2 * u32::from(params.k())
        } else {
            u32::from(params.k())
        };
        radix::sort(
            &mut scratch,
            &mut entries,
            bits,
            |entry| {
                if number == 3 {
                    entry.meta
                } else {
                    u64::from(entry.info)
                }
            },
            cancelled,
        )?;
        if profile {
            eprintln!(
                "{profile_name} sort_{number} seconds={:.3}",
                started.elapsed().as_secs_f64()
            );
        }
        std::mem::swap(&mut entries, &mut scratch);
        counts[number as usize] = entries.len();
    }
    drop(scratch);
    let mut fragments = allocate(entries.len())?;
    for entries in entries.chunks(BATCH) {
        check_cancelled(cancelled)?;
        fragments.extend(entries.iter().map(|entry| entry.meta));
    }
    check_cancelled(cancelled)?;
    Ok(CompactPlot {
        params,
        fragments,
        table_counts: counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Response {
        Echo,
        Failure,
        Length(usize),
    }

    struct TestEngine {
        response: Response,
        calls: usize,
    }

    impl HashEngine for TestEngine {
        fn is_accelerated(&self) -> bool {
            true
        }

        fn hash(
            &mut self,
            inputs: &[[u32; 4]],
            _rounds: u32,
            _cancelled: &AtomicBool,
        ) -> Result<Vec<[u32; 4]>, Error> {
            self.calls += 1;
            match self.response {
                Response::Echo => Ok(inputs.to_vec()),
                Response::Failure => Err(Error::new(ErrorKind::Unsupported, "test accelerator")),
                Response::Length(length) => Ok(vec![[0; 4]; length]),
            }
        }
    }

    fn params() -> ProofParams {
        ProofParams::new([37; 32].into(), 28, 2, false).unwrap()
    }

    fn limits() -> PlotLimits {
        PlotLimits {
            memory_bytes: 12 * 1024 * 1024 * 1024,
            max_entries: 310_000_000,
            max_work: 100_000_000_000,
        }
    }

    #[test]
    fn k28_preflight_rejects_cancelled_and_insufficient_budgets() {
        let params = params();
        let limits = limits();
        let required = CompactPlot::memory_required(28, limits.max_entries).unwrap();
        let error = build(params.clone(), limits, &AtomicBool::new(true))
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::Interrupted);
        for invalid in [
            PlotLimits {
                memory_bytes: required - 1,
                ..limits
            },
            PlotLimits {
                max_work: CompactPlot::minimum_work(&params) - 1,
                ..limits
            },
            PlotLimits {
                max_entries: (1 << 28) - 1,
                ..limits
            },
        ] {
            let error = build(params.clone(), invalid, &AtomicBool::new(false))
                .err()
                .unwrap();
            assert_eq!(error.kind(), ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn k28_hybrid_scratch_is_budgeted_before_engine_execution() {
        let mut engine = TestEngine {
            response: Response::Echo,
            calls: 0,
        };
        let limits = limits();
        let required = CompactPlot::memory_required(28, limits.max_entries).unwrap();
        let error = build_with_engine(
            params(),
            PlotLimits {
                memory_bytes: required + GPU_SCRATCH_BYTES - 1,
                ..limits
            },
            &AtomicBool::new(false),
            &mut engine,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(engine.calls, 0);
    }

    #[test]
    fn accelerator_failures_and_wrong_lengths_do_not_fall_back() {
        let inputs = [[1, 2, 3, 4], [5, 6, 7, 8]];
        let cancelled = AtomicBool::new(false);
        for response in [Response::Failure, Response::Length(1), Response::Length(3)] {
            let mut engine = TestEngine { response, calls: 0 };
            let mut output = [[u32::MAX; 4]; 2];
            {
                let backend = Backend::Accelerator(Mutex::new(&mut engine));
                let error = backend
                    .hash(&inputs, 16, &mut output, &cancelled)
                    .unwrap_err();
                assert_eq!(
                    error.kind(),
                    match response {
                        Response::Failure => ErrorKind::Unsupported,
                        _ => ErrorKind::InvalidData,
                    }
                );
            }
            assert_eq!(engine.calls, 1);
            assert_eq!(output, [[u32::MAX; 4]; 2]);
        }
    }

    #[test]
    fn accelerator_cancellation_precedes_execution() {
        let mut engine = TestEngine {
            response: Response::Echo,
            calls: 0,
        };
        let mut output = [[u32::MAX; 4]];
        {
            let backend = Backend::Accelerator(Mutex::new(&mut engine));
            let error = backend
                .hash(&[[1, 2, 3, 4]], 16, &mut output, &AtomicBool::new(true))
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Interrupted);
        }
        assert_eq!(engine.calls, 0);
        assert_eq!(output, [[u32::MAX; 4]]);
    }

    #[test]
    fn parallel_context_preserves_work_budget_on_rejection() {
        fn assert_send_sync<Value: Send + Sync>() {}
        assert_send_sync::<Backend<'static>>();
        assert_send_sync::<Context<'static, 'static>>();

        let params = params();
        let cancelled = AtomicBool::new(false);
        let context = Context {
            configuration: config(&params),
            backend: Backend::Cpu(CpuHasher::new(&params)),
            remaining: AtomicU64::new(3),
            cancelled: &cancelled,
            capacity: 2,
            batch_size: 2,
        };
        context.charge(2).unwrap();
        assert_eq!(context.remaining.load(Ordering::Relaxed), 1);
        assert!(context.charge(2).is_err());
        assert_eq!(context.remaining.load(Ordering::Relaxed), 1);
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(
            context.charge(1).unwrap_err().kind(),
            ErrorKind::Interrupted
        );
        assert_eq!(context.remaining.load(Ordering::Relaxed), 1);
    }
}
