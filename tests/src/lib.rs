use tokio::time::Instant;
use dg_xch_pos::verifier::check_plot;

mod f_calc;
mod prover;

#[test]
pub fn speed_test_check_plot() {
    use log::info;
    use simple_logger::SimpleLogger;
    SimpleLogger::new().env().init().unwrap();
    let run_amt = 500;
    let start = Instant::now();
    let path ="/mnt/96acc2b7-d09d-4d27-a09c-a8b425a59813/plot-k32-2023-05-30-23-09-003fd7e478ccf85bddf96300461963bc9543e7b9cc0360ba429c40c5f0757edf.plot";
    let (total, bad) = check_plot(path, run_amt).unwrap();
    let time = Instant::now().duration_since(start).as_millis();
    let seconds = time as f64 / 1000.0;
    let avg = seconds / run_amt as f64;
    info!("Proofs Found: {total}/{run_amt}, Bad Proofs: {bad}, took {seconds} seconds, avg: {avg}");
}

