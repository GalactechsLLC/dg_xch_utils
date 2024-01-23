use criterion::Criterion;
use dg_xch_core::plots::PlotFile;
use dg_xch_pos::constants::ucdiv_t;
use dg_xch_pos::plots::decompressor::DecompressorPool;
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::PlotReader;
use simple_logger::SimpleLogger;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::available_parallelism;
use tokio::runtime::{Builder, Runtime};

fn proof_benchmark(c: &mut Criterion, runtime: &Runtime) {
    SimpleLogger::new().env().init().unwrap_or_default();
    let path = Path::new("/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot");
    let pool = Arc::new(DecompressorPool::new(
        1,
        available_parallelism().map(|u| u.get()).unwrap_or(4) as u8,
    ));
    let plot_reader = runtime.block_on(async {
        PlotReader::new(
            DiskPlot::new(path).await.unwrap(),
            Some(pool.clone()),
            Some(pool),
        )
        .await
        .unwrap()
    });
    let reader = Arc::new(plot_reader);
    let f7 = Arc::new(AtomicU64::new(0));
    c.bench_function("Proof Bench", |b| {
        let reader = reader.clone();
        b.to_async(runtime).iter(|| async {
            let mut challenge =
                hex::decode("00000000ff04b8ee9355068689bd558eafe07cc7af47ad1574b074fc34d6913a")
                    .unwrap();
            let _f7 = f7.load(Ordering::Relaxed);
            let f7size = ucdiv_t(reader.plot_file().k() as usize, 8);
            for (i, v) in challenge[0..f7size].iter_mut().enumerate() {
                *v = (_f7 >> ((f7size - i - 1) * 8)) as u8;
            }
            let _ = reader.fetch_proofs_for_challenge(&challenge).await;
            f7.fetch_add(1, Ordering::Relaxed);
        })
    });
}

fn quality_then_proof_benchmark(c: &mut Criterion, runtime: &Runtime) {
    SimpleLogger::new().env().init().unwrap_or_default();
    let path = Path::new("/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot");
    let pool = Arc::new(DecompressorPool::new(
        1,
        available_parallelism().map(|u| u.get()).unwrap_or(4) as u8,
    ));
    let plot_reader = runtime.block_on(async {
        PlotReader::new(
            DiskPlot::new(path).await.unwrap(),
            Some(pool.clone()),
            Some(pool),
        )
        .await
        .unwrap()
    });
    let reader = Arc::new(plot_reader);
    let f7 = Arc::new(AtomicU64::new(0));
    c.bench_function("Quality + Proof Bench", |b| {
        let reader = reader.clone();
        b.to_async(runtime).iter(|| async {
            let mut challenge =
                hex::decode("00000000ff04b8ee9355068689bd558eafe07cc7af47ad1574b074fc34d6913a")
                    .unwrap();
            let _f7 = f7.load(Ordering::Relaxed);
            let f7size = ucdiv_t(reader.plot_file().k() as usize, 8);
            for (i, v) in challenge[0..f7size].iter_mut().enumerate() {
                *v = (_f7 >> ((f7size - i - 1) * 8)) as u8;
            }
            for (index, _) in reader
                .fetch_qualities_for_challenge(&challenge)
                .await
                .unwrap_or_default()
            {
                let _ = reader.fetch_ordered_proof(index).await;
            }
            f7.fetch_add(1, Ordering::Relaxed);
        })
    });
}

fn quality_benchmark(c: &mut Criterion, runtime: &Runtime) {
    SimpleLogger::new().env().init().unwrap_or_default();
    let path = Path::new("/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot");
    let pool = Arc::new(DecompressorPool::new(
        1,
        available_parallelism().map(|u| u.get()).unwrap_or(4) as u8,
    ));
    let plot_reader = runtime.block_on(async {
        PlotReader::new(
            DiskPlot::new(path).await.unwrap(),
            Some(pool.clone()),
            Some(pool),
        )
        .await
        .unwrap()
    });
    let reader = Arc::new(plot_reader);
    let f7 = Arc::new(AtomicU64::new(0));
    c.bench_function("Quality Bench", |b| {
        let reader = reader.clone();
        b.to_async(runtime).iter(|| async {
            let mut challenge =
                hex::decode("00000000ff04b8ee9355068689bd558eafe07cc7af47ad1574b074fc34d6913a")
                    .unwrap();
            let _f7 = f7.load(Ordering::Relaxed);
            let f7size = ucdiv_t(reader.plot_file().k() as usize, 8);
            for (i, v) in challenge[0..f7size].iter_mut().enumerate() {
                *v = (_f7 >> ((f7size - i - 1) * 8)) as u8;
            }
            reader
                .fetch_qualities_for_challenge(&challenge)
                .await
                .unwrap();
        })
    });
}

pub fn load_10_provers(runtime: Runtime) {
    let path = Path::new("/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot");
}

pub fn benches(runtime: Runtime) {
    let mut criterion = Criterion::default().configure_from_args();
    let mut criterion = criterion.sample_size(50);
    quality_benchmark(&mut criterion, &runtime);
    let mut criterion = criterion.sample_size(10);
    proof_benchmark(&mut criterion, &runtime);
    quality_then_proof_benchmark(&mut criterion, &runtime);
    criterion.final_summary();
}

fn main() {
    let runtime = Builder::new_multi_thread()
        .worker_threads(available_parallelism().map(|u| u.get()).unwrap_or(4))
        .thread_name("benchmark runtime")
        .build()
        .unwrap();
    benches(runtime);
}
