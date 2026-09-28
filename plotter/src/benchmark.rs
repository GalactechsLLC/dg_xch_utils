use crate::{format, inspect};
use blst::min_pk::SecretKey;
use dg_xch_core::blockchain::proof_of_space::{calculate_plot_id_v2, generate_plot_public_key};
use dg_xch_core::blockchain::sized_bytes::{Bytes32, Bytes48};
use dg_xch_keys::master_sk_to_local_sk;
use dg_xch_pos2::compact::CompactPlot;
use dg_xch_pos2::compute::{CpuHasher, HashEngine};
use dg_xch_pos2::params::ProofParams;
use dg_xch_pos2::plotting::PlotLimits;
use std::fs::File;
use std::io::{Error, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

struct BenchmarkIdentity {
    params: ProofParams,
    memo: Vec<u8>,
    index: u16,
    meta_group: u8,
}

enum BenchmarkPlot {
    Host(CompactPlot),
    #[cfg(feature = "vulkan")]
    Vulkan(crate::vulkan::Plot),
}

impl BenchmarkPlot {
    fn table_counts(&self) -> [usize; 4] {
        match self {
            Self::Host(plot) => plot.table_counts,
            #[cfg(feature = "vulkan")]
            Self::Vulkan(plot) => plot.table_counts(),
        }
    }

    fn write(
        &self,
        output: &mut File,
        fixture: &BenchmarkIdentity,
        memory_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<(), Error> {
        match self {
            Self::Host(plot) => {
                let _ = memory_bytes;
                format::write_compact(
                    output,
                    plot,
                    fixture.index,
                    fixture.meta_group,
                    &fixture.memo,
                    cancelled,
                )
            }
            #[cfg(feature = "vulkan")]
            Self::Vulkan(plot) => plot.write(
                output,
                fixture.index,
                fixture.meta_group,
                &fixture.memo,
                memory_bytes,
                cancelled,
            ),
        }
    }
}

fn identity() -> Result<BenchmarkIdentity, Error> {
    if let Some(input) = std::env::var_os("DGX_POS2_BENCHMARK_INPUT") {
        let path = Path::new(&input);
        let info = inspect(path)?;
        if info.k != 28 || info.strength != 2 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "benchmark input must be a mainnet k28 strength-2 plot",
            ));
        }
        let mut file = File::open(path)?;
        let mut header = [0; 43];
        file.read_exact(&mut header)?;
        let mut memo = vec![0; usize::from(header[42])];
        file.read_exact(&mut memo)?;
        return Ok(BenchmarkIdentity {
            params: ProofParams::new(info.plot_id.into(), 28, 2, false)?,
            memo,
            index: info.index,
            meta_group: info.meta_group,
        });
    }
    let master = SecretKey::key_gen_v3(&[11; 32], &[])
        .map_err(|_| Error::other("benchmark master key generation failed"))?;
    let farmer = SecretKey::key_gen_v3(&[7; 32], &[])
        .map_err(|_| Error::other("benchmark farmer key generation failed"))?
        .sk_to_pk();
    let local = master_sk_to_local_sk(&master)?;
    let plot_key = generate_plot_public_key(&local.sk_to_pk(), &farmer, true)?;
    let index = 256;
    let meta_group = 3;
    let contract = [8; 32];
    let plot_id = calculate_plot_id_v2(
        2,
        Bytes48::from(plot_key.to_bytes()),
        None,
        Some(Bytes32::from(contract)),
        index,
        meta_group,
    );
    let mut memo = Vec::with_capacity(112);
    memo.extend_from_slice(&contract);
    memo.extend_from_slice(&farmer.to_bytes());
    memo.extend_from_slice(&master.to_bytes());
    Ok(BenchmarkIdentity {
        params: ProofParams::new(plot_id, 28, 2, false)?,
        memo,
        index,
        meta_group,
    })
}

struct TimedHasher {
    inner: Box<dyn HashEngine>,
    elapsed: Duration,
    batches: u64,
    inputs: u64,
    evaluations: u64,
}

impl HashEngine for TimedHasher {
    fn is_accelerated(&self) -> bool {
        self.inner.is_accelerated()
    }

    fn build_compact(
        &mut self,
        params: &ProofParams,
        limits: PlotLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<CompactPlot>, Error> {
        self.inner.build_compact(params, limits, cancelled)
    }

    fn hash(
        &mut self,
        inputs: &[[u32; 4]],
        rounds: u32,
        cancelled: &AtomicBool,
    ) -> Result<Vec<[u32; 4]>, Error> {
        let started = Instant::now();
        let result = self.inner.hash(inputs, rounds, cancelled);
        self.elapsed += started.elapsed();
        self.batches += 1;
        self.inputs += inputs.len() as u64;
        self.evaluations += inputs.len() as u64 * u64::from(rounds / 16);
        result
    }
}

#[test]
#[ignore = "full k28 plotting benchmark; requires explicit output path and release mode"]
fn k28_plotting() -> Result<(), Error> {
    if cfg!(debug_assertions) {
        return Err(Error::other("run plotting benchmarks in release mode"));
    }
    let destination = PathBuf::from(
        std::env::var_os("DGX_POS2_BENCHMARK_OUTPUT")
            .ok_or_else(|| Error::other("set DGX_POS2_BENCHMARK_OUTPUT to a new plot path"))?,
    );
    if destination.exists() {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "benchmark destination already exists",
        ));
    }
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let fixture = identity()?;
    let backend = std::env::var("DGX_POS2_BENCHMARK_BACKEND").unwrap_or_else(|_| "cpu".into());
    let limits = PlotLimits {
        memory_bytes: 12 * 1024 * 1024 * 1024,
        max_entries: 310_000_000,
        max_work: 100_000_000_000,
    };
    let cancelled = AtomicBool::new(false);
    let total_started = Instant::now();
    #[cfg(feature = "vulkan")]
    let mut vulkan_ordinal = None;
    let engine: Option<Box<dyn HashEngine>> = match backend.as_str() {
        "cpu" | "cpu-batched" => Some(Box::new(CpuHasher::new(&fixture.params))),
        #[cfg(feature = "vulkan")]
        "vulkan" => {
            let device = std::env::var("DGX_POS2_BENCHMARK_DEVICE")
                .map_err(|_| Error::other("set DGX_POS2_BENCHMARK_DEVICE explicitly for Vulkan"))?
                .parse::<usize>()
                .map_err(|_| Error::other("invalid Vulkan device ordinal"))?;
            let adapter = dg_xch_pos2::vulkan::adapters()
                .into_iter()
                .find(|adapter| adapter.ordinal == device)
                .ok_or_else(|| Error::other("Vulkan benchmark device was not found"))?;
            println!(
                "benchmark_device={} vendor={:#x}",
                adapter.name, adapter.vendor
            );
            vulkan_ordinal = Some(device);
            None
        }
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "benchmark backend must be cpu, cpu-batched or an enabled Vulkan backend",
            ));
        }
    };
    let setup_seconds = total_started.elapsed().as_secs_f64();
    let mut engine = engine.map(|engine| TimedHasher {
        inner: engine,
        elapsed: Duration::ZERO,
        batches: 0,
        inputs: 0,
        evaluations: 0,
    });
    let build_started = Instant::now();
    let plot = if backend == "cpu" {
        BenchmarkPlot::Host(CompactPlot::build(
            fixture.params.clone(),
            limits,
            &cancelled,
        )?)
    } else if let Some(engine) = engine.as_mut() {
        BenchmarkPlot::Host(CompactPlot::build_with_engine(
            fixture.params.clone(),
            limits,
            &cancelled,
            engine,
        )?)
    } else {
        #[cfg(feature = "vulkan")]
        {
            BenchmarkPlot::Vulkan(crate::vulkan::Plot::build(
                fixture.params.clone(),
                vulkan_ordinal.ok_or_else(|| Error::other("missing Vulkan benchmark ordinal"))?,
                limits,
                &cancelled,
            )?)
        }
        #[cfg(not(feature = "vulkan"))]
        return Err(Error::other("missing benchmark engine"));
    };
    let build_seconds = build_started.elapsed().as_secs_f64();
    let hash_profiled =
        backend != "cpu" && engine.as_ref().is_some_and(|engine| engine.batches != 0);
    let write_started = Instant::now();
    let mut temporary = tempfile::Builder::new()
        .prefix(".pos2-benchmark-")
        .suffix(".partial")
        .tempfile_in(parent)?;
    plot.write(
        temporary.as_file_mut(),
        &fixture,
        limits.memory_bytes,
        &cancelled,
    )?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    let write_sync_seconds = write_started.elapsed().as_secs_f64();
    let total_seconds = total_started.elapsed().as_secs_f64();
    let info = inspect(&destination)?;
    assert_eq!(Bytes32::from(info.plot_id), fixture.params.plot_id());
    assert_eq!(info.k, 28);
    assert_eq!(info.strength, 2);
    println!(
        "benchmark backend={backend} k=28 strength=2 testnet=false plot_id={} bytes={} chunks={} table_counts={:?} memory_budget_bytes={} max_entries={} max_work={} rayon_threads={}",
        hex::encode(info.plot_id),
        info.file_bytes,
        info.chunks,
        plot.table_counts(),
        limits.memory_bytes,
        limits.max_entries,
        limits.max_work,
        std::env::var("RAYON_NUM_THREADS").unwrap_or_else(|_| "automatic".into())
    );
    println!(
        "benchmark_seconds setup={setup_seconds:.6} build={build_seconds:.6} write_sync={write_sync_seconds:.6} total={total_seconds:.6} hash_profiled={hash_profiled} output={}",
        destination.display()
    );
    if let Some(engine) = engine.as_ref().filter(|_| hash_profiled) {
        println!(
            "benchmark_hash_seconds calls={:.6} wall_outside_hash_calls={:.6}",
            engine.elapsed.as_secs_f64(),
            build_seconds - engine.elapsed.as_secs_f64()
        );
        println!(
            "benchmark_work batches={} inputs={} sixteen_round_evaluations={}",
            engine.batches, engine.inputs, engine.evaluations
        );
    }
    Ok(())
}
