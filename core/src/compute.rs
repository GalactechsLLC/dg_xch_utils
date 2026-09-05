use rayon::prelude::*;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy)]
pub enum Phase {
    Vdf,
    Signature,
    Body,
    Archive,
    CoinPrepare,
}

impl Phase {
    pub const ALL: [Self; 5] = [
        Self::Vdf,
        Self::Signature,
        Self::Body,
        Self::Archive,
        Self::CoinPrepare,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Vdf => "vdf",
            Self::Signature => "signature",
            Self::Body => "body",
            Self::Archive => "archive",
            Self::CoinPrepare => "coin_prepare",
        }
    }
}

#[derive(Default)]
pub struct Counters {
    pub active: AtomicU64,
    pub pending: AtomicU64,
    pub completed: AtomicU64,
    pub elapsed_nanos: AtomicU64,
    pub cpu_nanos: AtomicU64,
    pub wait_nanos: AtomicU64,
}

static COUNTERS: [Counters; 5] = [const {
    Counters {
        active: AtomicU64::new(0),
        pending: AtomicU64::new(0),
        completed: AtomicU64::new(0),
        elapsed_nanos: AtomicU64::new(0),
        cpu_nanos: AtomicU64::new(0),
        wait_nanos: AtomicU64::new(0),
    }
}; 5];
struct ComputePools {
    validation: rayon::ThreadPool,
    archive: Option<rayon::ThreadPool>,
    workers: usize,
}

impl ComputePools {
    fn new(workers: usize) -> Result<Self, rayon::ThreadPoolBuildError> {
        let archive_workers = usize::from(workers > 1);
        let validation = rayon::ThreadPoolBuilder::new()
            .num_threads(workers - archive_workers)
            .thread_name(|index| format!("dg-compute-{index}"))
            .build()?;
        let archive = if archive_workers == 0 {
            None
        } else {
            Some(
                rayon::ThreadPoolBuilder::new()
                    .num_threads(archive_workers)
                    .thread_name(|index| format!("dg-archive-{index}"))
                    .build()?,
            )
        };
        Ok(Self {
            validation,
            archive,
            workers,
        })
    }

    fn for_phase(&self, phase: Phase) -> &rayon::ThreadPool {
        match phase {
            Phase::Archive | Phase::CoinPrepare => {
                self.archive.as_ref().unwrap_or(&self.validation)
            }
            _ => &self.validation,
        }
    }
}

static POOL: OnceLock<ComputePools> = OnceLock::new();

pub fn counters(phase: Phase) -> &'static Counters {
    &COUNTERS[phase as usize]
}

pub fn configure(workers: usize) -> Result<(), String> {
    if workers == 0 {
        return Err("compute workers must be positive".into());
    }
    if let Some(pool) = POOL.get() {
        return if pool.workers == workers {
            Ok(())
        } else {
            Err("compute pool is already initialized with a different worker count".into())
        };
    }
    let pool = ComputePools::new(workers).map_err(|error| error.to_string())?;
    POOL.set(pool)
        .map_err(|_| "compute pool was initialized concurrently".into())
}

fn pool() -> &'static ComputePools {
    POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism()
            .map_or(4, std::num::NonZeroUsize::get)
            .saturating_sub(2)
            .max(1);
        ComputePools::new(workers).expect("create compute workers")
    })
}

pub fn worker_count() -> usize {
    pool().workers
}

pub fn thread_cpu_nanos() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value) } == 0 {
            return (value.tv_sec as u64)
                .saturating_mul(1_000_000_000)
                .saturating_add(value.tv_nsec as u64);
        }
    }
    0
}

struct ActiveJob {
    counters: &'static Counters,
    started: Instant,
    cpu_started: u64,
}

impl Drop for ActiveJob {
    fn drop(&mut self) {
        self.counters.active.fetch_sub(1, Ordering::Relaxed);
        self.counters.completed.fetch_add(1, Ordering::Relaxed);
        self.counters
            .elapsed_nanos
            .fetch_add(self.started.elapsed().as_nanos() as u64, Ordering::Relaxed);
        self.counters.cpu_nanos.fetch_add(
            thread_cpu_nanos().saturating_sub(self.cpu_started),
            Ordering::Relaxed,
        );
    }
}

pub fn map<Input: Sync, Output: Send>(
    phase: Phase,
    inputs: &[Input],
    operation: impl Fn(&Input) -> Output + Send + Sync,
) -> Vec<Output> {
    if inputs.is_empty() {
        return Vec::new();
    }
    let counters = counters(phase);
    let submitted = Instant::now();
    counters
        .pending
        .fetch_add(inputs.len() as u64, Ordering::Relaxed);
    let pending = PendingBatch {
        counters,
        remaining: AtomicU64::new(inputs.len() as u64),
    };
    pool().for_phase(phase).install(|| {
        inputs
            .par_iter()
            .map(|input| {
                pending.remaining.fetch_sub(1, Ordering::Relaxed);
                counters.pending.fetch_sub(1, Ordering::Relaxed);
                counters
                    .wait_nanos
                    .fetch_add(submitted.elapsed().as_nanos() as u64, Ordering::Relaxed);
                counters.active.fetch_add(1, Ordering::Relaxed);
                let _active = ActiveJob {
                    counters,
                    started: Instant::now(),
                    cpu_started: thread_cpu_nanos(),
                };
                operation(input)
            })
            .collect()
    })
}

struct PendingBatch {
    counters: &'static Counters,
    remaining: AtomicU64,
}

impl Drop for PendingBatch {
    fn drop(&mut self) {
        self.counters
            .pending
            .fetch_sub(self.remaining.load(Ordering::Relaxed), Ordering::Relaxed);
    }
}
