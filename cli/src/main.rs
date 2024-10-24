#[tokio::main]
async fn main() -> Result<(), std::io::Error> {
    dg_xch_cli_lib::run_cli().await
}
