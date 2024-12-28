use dg_xch_simulator_lib::start_simulator;

#[tokio::main]
pub async fn main() -> Result<(), std::io::Error> {
    start_simulator().await
}
