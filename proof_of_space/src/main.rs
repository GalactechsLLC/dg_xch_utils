use dg_xch_core::plots::PlotFile;
use dg_xch_pos::constants::ucdiv_t;
use dg_xch_pos::plots::decompressor::DecompressorPool;
use dg_xch_pos::plots::disk_plot::DiskPlot;
use dg_xch_pos::plots::plot_reader::PlotReader;
use dg_xch_pos::verifier::proof_to_bytes;
use log::info;
use simple_logger::SimpleLogger;
use std::io::Error;
use std::path::Path;
use std::sync::Arc;
use std::thread::available_parallelism;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Error> {
    SimpleLogger::new().env().init().unwrap();
    let path = Path::new("/home/luna/plot-k32-c05-2023-06-09-02-25-11d916cf9c847158f76affb30a38ca36f83da452c37f4b4d10a1a0addcfa932b.plot");
    let mut challenge =
        hex::decode("00000000ff04b8ee9355068689bd558eafe07cc7af47ad1574b074fc34d6913a").unwrap();
    let start = Instant::now();
    let pool = Arc::new(DecompressorPool::new(
        1,
        available_parallelism().map(|u| u.get()).unwrap_or_default() as u8,
    ));
    let plot_reader = PlotReader::new(
        DiskPlot::new(path).await.unwrap(),
        Some(pool.clone()),
        Some(pool),
    )
    .await
    .unwrap();
    let k = *plot_reader.plot_file().k();
    for f7 in 0..1 {
        if f7 >= 0 {
            let f7size = ucdiv_t(k as usize, 8);
            for (i, v) in challenge[0..f7size].iter_mut().enumerate() {
                *v = (f7 >> ((f7size - i - 1) * 8)) as u8;
            }
        }
        let proofs = match plot_reader.fetch_proof_for_challenge(&challenge).await {
            Ok(q) => q,
            Err(e) => {
                info!("Failed to Find Proof for f7({f7}): {:?}", e);
                continue;
            }
        };
        for proof in proofs {
            info!("Found Proof: {}", hex::encode(proof_to_bytes(&proof)));
        }
    }
    let dur = Instant::now().duration_since(start);
    info!("Took: {} seconds", dur.as_millis() as f64 / 1000.0);
    Ok(())
}
